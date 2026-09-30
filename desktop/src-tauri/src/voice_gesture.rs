use crate::config::{MagicianDesktopConfig, DEFAULT_OVERLAY_GESTURE};
use std::time::{Duration, Instant};
use tauri::AppHandle;

#[derive(Default)]
pub struct VoiceGestureState {
    #[cfg(target_os = "macos")]
    handle: Option<macos::GestureTapHandle>,
    #[cfg(target_os = "macos")]
    spec: Option<macos::VoiceGestureSpec>,
}

pub fn validate_voice_gestures(config: &MagicianDesktopConfig) -> Result<(), String> {
    parse_overlay_gesture(&config.general.quick_overlay_gesture)?;
    Ok(())
}

pub async fn sync_voice_gestures(
    app: &AppHandle,
    config: &MagicianDesktopConfig,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::sync_voice_gestures(app, config).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, config);
        Ok(())
    }
}

/// Display label for the quick-overlay gesture (e.g. "Double Left ⌥").
///
/// Pure: the same string on every machine. The permission-aware variant is
/// `overlay_gesture_menu_label`, kept separate so this stays testable without
/// depending on the Accessibility state of whatever runs the tests.
pub fn overlay_gesture_label(gesture: &str) -> Option<String> {
    overlay_modifier_label(gesture).map(|key| format!("Double {key}"))
}

/// The modifier named by the overlay gesture, without the double-tap word.
///
/// "Double Left Option" is "Left ⌥". Disabled and unknown values are `None`.
pub fn overlay_modifier_label(gesture: &str) -> Option<String> {
    parse_overlay_gesture(gesture)
        .ok()
        .flatten()
        .map(|key| key.label().to_string())
}

/// The label as the tray should show it.
///
/// The tray advertises this gesture as the primary trigger, so a menu that
/// promises "Double Left ⌥" while the tap could never be created is how this
/// failure went unnoticed for eleven days after the bundle identifier changed
/// and orphaned the old TCC grant.
pub fn overlay_gesture_menu_label(gesture: &str) -> Option<String> {
    let label = overlay_gesture_label(gesture)?;
    if accessibility_trusted() {
        Some(label)
    } else {
        Some(format!("{label} — needs Accessibility"))
    }
}

/// Whether macOS trusts this process for Accessibility.
///
/// The quick-overlay tap is a `CGEventTapOptions::Default` (active) tap, which
/// requires Accessibility rather than the weaker Input Monitoring grant.
pub fn accessibility_trusted() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::ax_trusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Ask macOS to attribute an Accessibility request to this signed desktop app.
///
/// The returned value is the current trust state. macOS may return `false`
/// while presenting its consent dialog; callers should refresh after the user
/// responds.
pub fn request_accessibility_access() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::prompt_for_accessibility()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// The quick overlay is summoned by a double-tap of a single modifier key
/// (default Left Option, replacing the Cmd+Z chord that collided with the
/// system-wide Undo). Returns the key, or None when disabled.
fn parse_overlay_gesture(raw: &str) -> Result<Option<KeySelector>, String> {
    let normalized = normalize_gesture_text(raw, DEFAULT_OVERLAY_GESTURE);
    if is_disabled(&normalized) {
        return Ok(None);
    }
    match normalized.as_str() {
        "double left option" | "double leftoption" | "left option left option"
        | "leftoption leftoption" | "double option" | "double opt" => {
            Ok(Some(KeySelector::LeftOption))
        },
        "double right option" | "double rightoption" | "right option right option"
        | "rightoption rightoption" => Ok(Some(KeySelector::RightOption)),
        _ => Err(format!(
            "Unsupported overlay gesture '{raw}'. Use Double Left Option, Double Right Option, or Disabled."
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeySelector {
    LeftOption,
    RightOption,
}

impl KeySelector {
    fn label(self) -> &'static str {
        match self {
            Self::LeftOption => "Left ⌥",
            Self::RightOption => "Right ⌥",
        }
    }

    #[cfg(target_os = "macos")]
    fn matches_keycode(self, keycode: u16) -> bool {
        use core_graphics::event::KeyCode;
        match self {
            Self::LeftOption => keycode == KeyCode::OPTION,
            Self::RightOption => keycode == KeyCode::RIGHT_OPTION,
        }
    }
}

fn normalize_gesture_text(raw: &str, default_value: &str) -> String {
    let raw = raw.trim();
    let value = if raw.is_empty() { default_value } else { raw };
    value
        .replace('⌥', " option ")
        .replace('⌃', " control ")
        .replace('⇧', " shift ")
        .replace('⌘', " command ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn is_disabled(value: &str) -> bool {
    matches!(value, "disabled" | "none" | "off")
}

const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(450);
const LONG_PRESS: Duration = Duration::from_millis(420);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldUp {
    Tap,
    DoubleTap,
    Release,
}

/// Distinguishes a short Left Option tap (and a second tap that opens the HUD)
/// from a long press that talks through the Orb.
#[derive(Debug, Clone)]
struct OverlayHoldTracker {
    last_tap: Option<Instant>,
    generation: u64,
    holding: bool,
}

impl Default for OverlayHoldTracker {
    fn default() -> Self {
        Self {
            last_tap: None,
            generation: 0,
            holding: false,
        }
    }
}

impl OverlayHoldTracker {
    /// A clean key-down. Returns the generation the long-press timer must echo.
    fn note_clean_down(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.holding = false;
        self.generation
    }

    /// The long-press timer fired. True only if this key is still held and no
    /// newer press, chord, or release has superseded it.
    fn note_timer(&mut self, generation: u64) -> bool {
        if self.generation != generation || self.holding {
            return false;
        }
        self.holding = true;
        self.last_tap = None;
        true
    }

    fn note_clean_up(&mut self, now: Instant) -> HoldUp {
        self.generation = self.generation.wrapping_add(1);
        if self.holding {
            self.holding = false;
            self.last_tap = None;
            return HoldUp::Release;
        }
        if let Some(last) = self.last_tap {
            if now.saturating_duration_since(last) <= DOUBLE_TAP_WINDOW {
                self.last_tap = None;
                return HoldUp::DoubleTap;
            }
        }
        self.last_tap = Some(now);
        HoldUp::Tap
    }

    /// A chord, another key, or a lost tap. Returns whether a hold that had
    /// already opened the microphone must be released.
    fn cancel(&mut self) -> bool {
        self.generation = self.generation.wrapping_add(1);
        self.last_tap = None;
        if self.holding {
            self.holding = false;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_gesture_labels_cover_supported_values_and_disabled() {
        assert_eq!(overlay_gesture_label(""), Some("Double Left ⌥".to_string()));
        assert_eq!(
            overlay_gesture_label("Double Right Option"),
            Some("Double Right ⌥".to_string())
        );
        assert_eq!(overlay_gesture_label("Disabled"), None);
    }

    #[test]
    fn the_plain_label_never_depends_on_the_hosts_accessibility_state() {
        // The menu label may or may not carry the permission suffix depending
        // on the machine; the plain one must not, or this test would assert
        // whether the runner happens to hold an Accessibility grant.
        assert_eq!(overlay_gesture_label(""), Some("Double Left ⌥".to_string()));
        let menu = overlay_gesture_menu_label("").expect("configured gesture");
        assert!(menu.starts_with("Double Left ⌥"));
        assert_eq!(
            menu.contains("needs Accessibility"),
            !accessibility_trusted()
        );
        assert_eq!(overlay_gesture_menu_label("Disabled"), None);
    }

    #[test]
    fn config_validation_ignores_retired_voice_gesture_but_validates_overlay() {
        let mut config = MagicianDesktopConfig::default();
        config.voice.voice_note_gesture = "unsupported legacy value".to_string();
        assert!(validate_voice_gestures(&config).is_ok());

        config.general.quick_overlay_gesture = "unsupported".to_string();
        assert!(validate_voice_gestures(&config).is_err());
    }

    #[test]
    fn a_short_double_tap_opens_the_hud_and_a_long_press_talks() {
        let mut tracker = OverlayHoldTracker::default();
        let start = Instant::now();
        let first = tracker.note_clean_down();
        assert!(!tracker.note_timer(first.wrapping_sub(1)));
        assert_eq!(tracker.note_clean_up(start), HoldUp::Tap);
        assert!(!tracker.holding);

        let second = tracker.note_clean_down();
        assert_eq!(
            tracker.note_clean_up(start + Duration::from_millis(200)),
            HoldUp::DoubleTap
        );
        assert!(!tracker.note_timer(second));

        let held = tracker.note_clean_down();
        assert!(tracker.note_timer(held));
        assert_eq!(
            tracker.note_clean_up(start + Duration::from_millis(800)),
            HoldUp::Release
        );
        assert!(!tracker.holding);
    }

    #[test]
    fn a_chord_cancels_a_pending_hold_and_releases_one_already_talking() {
        let mut tracker = OverlayHoldTracker::default();
        let pending = tracker.note_clean_down();
        assert!(!tracker.cancel());
        assert!(!tracker.note_timer(pending));

        let held = tracker.note_clean_down();
        assert!(tracker.note_timer(held));
        assert!(tracker.cancel());
        assert!(!tracker.holding);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{parse_overlay_gesture, KeySelector, OverlayHoldTracker, LONG_PRESS};
    use crate::{config::MagicianDesktopConfig, AppState};
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::mach_port::CFMachPortRef;
    use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
    use core_foundation::string::{CFString, CFStringRef};
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventTapProxy, CGEventType, CallbackResult, EventField,
    };
    use std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc, Arc, Mutex,
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };
    use tauri::{AppHandle, Manager};
    use tracing::{info, warn};

    // `core-graphics` only uses `CGEventTapEnable` internally (via
    // `CGEventTap::enable`) and doesn't re-export it, but we must re-enable the tap
    // from inside the callback after macOS disables it — so re-declare the framework
    // FFI. The symbol is linked via the `core-graphics` crate's CoreGraphics dep.
    extern "C" {
        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    }

    // Accessibility trust. `AXIsProcessTrusted` reports; the `WithOptions`
    // variant is the only call that makes macOS show the "open System
    // Settings" dialog, and it only does so once per app identity.
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
        static kAXTrustedCheckOptionPrompt: CFStringRef;
    }

    pub(super) fn ax_trusted() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }

    /// Ask macOS to show the Accessibility prompt, once per process.
    ///
    /// Without this the tap simply fails and the gesture is dead with no user
    /// signal — the OS never asks, because creating a tap is not a request.
    /// `contextual_assist` already gates its AX work on `AXIsProcessTrusted`;
    /// this module never did, which is why a permission the user would gladly
    /// have granted was never offered to them.
    pub(super) fn prompt_for_accessibility() -> bool {
        unsafe {
            let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
            let value = CFBoolean::true_value();
            let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
            AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) != 0
        }
    }

    fn prompt_for_accessibility_once() {
        static PROMPTED: AtomicBool = AtomicBool::new(false);
        if PROMPTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = prompt_for_accessibility();
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct VoiceGestureSpec {
        overlay: Option<KeySelector>,
    }

    impl VoiceGestureSpec {
        fn from_config(config: &MagicianDesktopConfig) -> Result<Self, String> {
            Ok(Self {
                // Quick-overlay summon: double-tap Left Option (replaces the Cmd+Z
                // chord that collided with the system-wide Undo).
                overlay: parse_overlay_gesture(&config.general.quick_overlay_gesture)?,
            })
        }

        fn is_empty(&self) -> bool {
            self.overlay.is_none()
        }
    }

    pub(super) struct GestureTapHandle {
        running: Arc<AtomicBool>,
        run_loop: CFRunLoop,
        join: Option<JoinHandle<()>>,
    }

    impl GestureTapHandle {
        fn stop(&mut self) {
            self.running.store(false, Ordering::SeqCst);
            self.run_loop.stop();
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    impl Drop for GestureTapHandle {
        fn drop(&mut self) {
            self.stop();
        }
    }

    struct TapRuntimeState {
        spec: VoiceGestureSpec,
        active_modifier_keys: HashSet<u16>,
        hold: OverlayHoldTracker,
    }

    pub(super) async fn sync_voice_gestures(
        app: &AppHandle,
        config: &MagicianDesktopConfig,
    ) -> Result<(), String> {
        let spec = VoiceGestureSpec::from_config(config)?;
        let app_state = app.state::<AppState>();
        let mut state = app_state.voice_gestures.lock().await;

        if state.spec.as_ref() == Some(&spec) {
            return Ok(());
        }

        if let Some(mut handle) = state.handle.take() {
            handle.stop();
        }
        state.spec = Some(spec.clone());

        if spec.is_empty() {
            info!("macOS voice gesture tap disabled");
            return Ok(());
        }

        // An active event tap needs Accessibility. Ask for it before trying,
        // so the user gets the system dialog instead of a silent no-op.
        if !ax_trusted() {
            prompt_for_accessibility_once();
        }

        match start_gesture_tap(app.clone(), spec.clone()) {
            Ok(handle) => {
                state.handle = Some(handle);
                info!("macOS voice gesture tap enabled");
                publish_gesture_status(app, true);
            },
            Err(error) => {
                warn!("macOS voice gesture tap unavailable: {}", error);
                // Clearing the spec lets the next sync retry from scratch.
                state.spec = None;
                publish_gesture_status(app, false);
                // Nothing else re-syncs, so without this the gesture stays
                // dead until an app restart even after the user grants the
                // permission the warning just asked for.
                spawn_trust_watcher(app.clone());
            },
        }
        Ok(())
    }

    /// Tell the rest of the app whether the gesture is live.
    ///
    /// A `warn!` in a log file is not a user-visible signal for the trigger the
    /// tray advertises as primary, so the tray label is rebuilt and the webview
    /// gets an event it can surface.
    fn publish_gesture_status(app: &AppHandle, enabled: bool) {
        use tauri::Emitter;
        let _ = app.emit(
            "voice-gesture-status",
            serde_json::json!({
                "enabled": enabled,
                "accessibilityTrusted": ax_trusted(),
                "permission": "accessibility",
            }),
        );
        // Rebuild so the "Quick Automate…" title stops promising a gesture
        // that cannot fire. `refresh_menu` reads the update-version indicator
        // from state, so passing None here does not clear it.
        let pending_update = {
            let state = app.state::<AppState>();
            state
                .pending_app_update
                .try_lock()
                .ok()
                .and_then(|pending| pending.clone())
        };
        crate::tray::refresh_menu(app, pending_update.as_deref());
    }

    /// Poll for the Accessibility grant and start the tap the moment it lands.
    ///
    /// Granting Accessibility does not restart the process and does not touch
    /// the desktop config, so no existing code path would ever retry. Bounded
    /// at ten minutes: past that the user has decided not to grant it, and a
    /// forever-poll is its own bug.
    fn spawn_trust_watcher(app: AppHandle) {
        static WATCHING: AtomicBool = AtomicBool::new(false);
        if WATCHING.swap(true, Ordering::SeqCst) {
            return;
        }
        tauri::async_runtime::spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(600);
            while Instant::now() < deadline {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if !ax_trusted() {
                    continue;
                }
                info!("Accessibility granted; re-arming the quick-overlay gesture");
                let config = {
                    let state = app.state::<AppState>();
                    let config = state.config.lock().await;
                    config.clone()
                };
                if let Err(error) = super::sync_voice_gestures(&app, &config).await {
                    warn!("re-arming the gesture after the grant failed: {error}");
                }
                break;
            }
            WATCHING.store(false, Ordering::SeqCst);
        });
    }

    fn start_gesture_tap(
        app: AppHandle,
        spec: VoiceGestureSpec,
    ) -> Result<GestureTapHandle, String> {
        let running = Arc::new(AtomicBool::new(true));
        let runtime = Arc::new(Mutex::new(TapRuntimeState {
            spec,
            active_modifier_keys: HashSet::new(),
            hold: OverlayHoldTracker::default(),
        }));
        // Shared mach-port pointer (as usize, since the tap callback must be `Send`)
        // so the callback can RE-ENABLE the tap after macOS disables it. Set once the
        // tap exists, below.
        let tap_port = Arc::new(AtomicUsize::new(0));
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_running = Arc::clone(&running);
        let join = thread::Builder::new()
            .name("magician-voice-gesture-tap".to_string())
            .spawn(move || {
                let run_loop = CFRunLoop::get_current();
                let callback_app = app.clone();
                let callback_runtime = Arc::clone(&runtime);
                let reenable_runtime = Arc::clone(&runtime);
                let callback_tap_port = Arc::clone(&tap_port);
                let event_tap = CGEventTap::new(
                    CGEventTapLocation::Session,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::Default,
                    vec![
                        CGEventType::FlagsChanged,
                        CGEventType::KeyDown,
                        CGEventType::LeftMouseDown,
                        CGEventType::RightMouseDown,
                        CGEventType::OtherMouseDown,
                    ],
                    move |_proxy: CGEventTapProxy, event_type: CGEventType, event: &CGEvent| {
                        // macOS disables an active event tap whenever the callback is
                        // slow or there's heavy input / a sleep-wake. If we don't
                        // RE-ENABLE it, the gesture silently dies until the app restarts
                        // — this is why double-tap Left Option "sometimes" stops opening
                        // the HUD while the OS global-shortcut chords (⇧⌥S) keep working.
                        // Re-enable, and drop modifier/tap state whose key-ups we missed
                        // while the tap was off.
                        if matches!(
                            event_type,
                            CGEventType::TapDisabledByTimeout
                                | CGEventType::TapDisabledByUserInput
                        ) {
                            let port = callback_tap_port.load(Ordering::SeqCst);
                            if port != 0 {
                                unsafe { CGEventTapEnable(port as CFMachPortRef, true) };
                            }
                            if let Ok(mut runtime) = reenable_runtime.lock() {
                                // We may have missed key-ups while the tap was off, so
                                // reset modifier and double-tap tracking. A hold that
                                // was already listening must close its microphone.
                                runtime.active_modifier_keys.clear();
                                if runtime.hold.cancel() {
                                    crate::voice_note::trigger_orb_hold_release(&callback_app);
                                }
                            }
                            warn!("voice gesture tap disabled by macOS; re-enabled");
                            return CallbackResult::Keep;
                        }
                        process_event(&callback_app, &callback_runtime, event_type, event)
                    },
                );

                let Ok(event_tap) = event_tap else {
                    let _ = ready_tx.send(Err("event tap creation failed; grant Input Monitoring/Accessibility permission to Magican Desktop".to_string()));
                    return;
                };

                // Publish the mach port so the callback can re-enable the tap.
                tap_port.store(
                    event_tap.mach_port().as_concrete_TypeRef() as usize,
                    Ordering::SeqCst,
                );

                let Ok(loop_source) = event_tap.mach_port().create_runloop_source(0) else {
                    let _ = ready_tx.send(Err("event tap runloop source creation failed".to_string()));
                    return;
                };

                run_loop.add_source(&loop_source, unsafe { kCFRunLoopCommonModes });
                event_tap.enable();
                let _ = ready_tx.send(Ok(run_loop.clone()));
                if thread_running.load(Ordering::SeqCst) {
                    CFRunLoop::run_current();
                }
            })
            .map_err(|error| format!("failed to spawn voice gesture tap thread: {error}"))?;

        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(run_loop)) => Ok(GestureTapHandle {
                running,
                run_loop,
                join: Some(join),
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            },
            Err(_) => Err("timed out starting voice gesture tap".to_string()),
        }
    }

    fn process_event(
        app: &AppHandle,
        runtime: &Arc<Mutex<TapRuntimeState>>,
        event_type: CGEventType,
        event: &CGEvent,
    ) -> CallbackResult {
        let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
        match event_type {
            CGEventType::FlagsChanged => {
                process_flags_changed(app, runtime, keycode, event.get_flags())
            },
            CGEventType::KeyDown => {
                // Option combined with a real key is a shortcut, not press-to-talk.
                // Leave the key itself alone so the destination app still receives it.
                let release = runtime.lock().ok().is_some_and(|mut runtime| {
                    let overlay_key = runtime
                        .spec
                        .overlay
                        .is_some_and(|selector| selector.matches_keycode(keycode));
                    !overlay_key && runtime.hold.cancel()
                });
                if release {
                    crate::voice_note::trigger_orb_hold_release(app);
                }
                CallbackResult::Keep
            },
            CGEventType::LeftMouseDown
            | CGEventType::RightMouseDown
            | CGEventType::OtherMouseDown => {
                let point = event.location();
                if crate::overlay::handle_draw_overlay_global_click(app, point.x, point.y) {
                    CallbackResult::Drop
                } else {
                    CallbackResult::Keep
                }
            },
            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                CallbackResult::Keep
            },
            _ => CallbackResult::Keep,
        }
    }

    fn process_flags_changed(
        app: &AppHandle,
        runtime: &Arc<Mutex<TapRuntimeState>>,
        keycode: u16,
        flags: CGEventFlags,
    ) -> CallbackResult {
        let runtime_handle = Arc::clone(runtime);
        let mut runtime = match runtime.lock() {
            Ok(runtime) => runtime,
            Err(_) => return CallbackResult::Keep,
        };
        let is_down = if runtime.active_modifier_keys.remove(&keycode) {
            false
        } else {
            runtime.active_modifier_keys.insert(keycode);
            true
        };

        let Some(selector) = runtime.spec.overlay else {
            return CallbackResult::Keep;
        };
        // "No other modifier held" — read from the LIVE flags so a stale
        // modifier (e.g. a missed Ctrl key-up) can't permanently block a clean
        // Left Option tap. Option itself is the gesture key, so it isn't part
        // of this check.
        let no_other_modifier = !flags.contains(CGEventFlags::CGEventFlagControl)
            && !flags.contains(CGEventFlags::CGEventFlagCommand)
            && !flags.contains(CGEventFlags::CGEventFlagShift);
        let overlay_key = selector.matches_keycode(keycode);

        if overlay_key && no_other_modifier && is_down {
            let generation = runtime.hold.note_clean_down();
            drop(runtime);
            arm_hold_timer(app.clone(), Arc::clone(&runtime_handle), generation);
            return CallbackResult::Keep;
        }

        if overlay_key && no_other_modifier && !is_down {
            match runtime.hold.note_clean_up(Instant::now()) {
                super::HoldUp::DoubleTap => {
                    drop(runtime);
                    crate::contextual_assist::suppress_contextual_assist_hotkey_for_overlay();
                    crate::overlay::trigger_overlay_toggle(app);
                },
                super::HoldUp::Release => {
                    drop(runtime);
                    crate::voice_note::trigger_orb_hold_release(app);
                },
                super::HoldUp::Tap => {},
            }
            return CallbackResult::Keep;
        }

        // A chord, or the overlay key coming up while another modifier is
        // down, is not a summon and must not leave the microphone open.
        if runtime.hold.cancel() {
            drop(runtime);
            crate::voice_note::trigger_orb_hold_release(app);
        }
        CallbackResult::Keep
    }

    fn arm_hold_timer(app: AppHandle, runtime: Arc<Mutex<TapRuntimeState>>, generation: u64) {
        thread::spawn(move || {
            thread::sleep(LONG_PRESS);
            let start = runtime
                .lock()
                .ok()
                .is_some_and(|mut runtime| runtime.hold.note_timer(generation));
            if start {
                crate::voice_note::trigger_orb_hold_start(&app);
            }
        });
    }
}
