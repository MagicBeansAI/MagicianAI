//! Native desktop wake word — Vosk in Rust (no Swift helper).
//!
//! Mirrors the browser's wake-word approach (`ui/.../wakeWord.ts`, vosk-browser)
//! but on the Tauri side: a `cpal` mic stream feeds a Vosk recognizer in a
//! background thread, and when the configured phrase appears in the transcript
//! it dispatches the process-owned orb lifecycle. On-device, free, offline, any name; and
//! because it runs in the Rust process it keeps listening when the window is
//! backgrounded (the reason the browser detector alone wasn't enough).
//!
//! The desktop Orb is the sole detector owner. Its device-local wake setting is
//! independent of the visible Orb lifecycle, so a phrase can cold-start a
//! hidden/off Orb. Accepted phrases never reach the web composer.
//!
//! **Gated behind the `native-wake` cargo feature** (off by default) so plain
//! `cargo build` / `cargo check` / tests don't require `libvosk`. The
//! desktop-wake build enables it (`make build-desktop-tray-debug` /
//! `release-desktop` pass `--features native-wake`); without the feature the
//! controller + commands still exist but the listener is a no-op.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use tauri::{AppHandle, Manager};
use tracing::warn;

/// Owns the running wake thread. Dropping (or `stop()`) signals + joins it.
struct WakeHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl WakeHandle {
    fn stop(mut self) {
        self.signal_and_join();
    }

    fn signal_and_join(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            // A wake hit hands off to the orb, which suspends this controller.
            // Never make correctness depend on the handoff racing this thread's
            // final return: dropping our own JoinHandle detaches safely.
            if join.thread().id() != std::thread::current().id() {
                let _ = join.join();
            }
        }
    }
}

impl Drop for WakeHandle {
    fn drop(&mut self) {
        self.signal_and_join();
    }
}

/// Settings-driven controller. The listener runs only while the device-local
/// Orb wake switch is enabled with a non-empty phrase.
#[derive(Default)]
pub struct WakeController {
    handle: Option<WakeHandle>,
    enabled: bool,
    suspended: bool,
    phrase: String,
}

impl WakeController {
    pub fn set_enabled(&mut self, app: &AppHandle, enabled: bool) {
        if self.enabled == enabled && !(enabled && self.handle.is_none()) {
            return;
        }
        self.enabled = enabled;
        if !enabled {
            self.suspended = false;
        }
        self.sync(app);
    }

    pub fn suspend(&mut self) {
        self.suspended = true;
        if let Some(handle) = self.handle.take() {
            handle.stop();
        }
    }

    pub fn resume(&mut self, app: &AppHandle) {
        if !self.enabled {
            return;
        }
        let needs_sync = self.suspended
            || self
                .handle
                .as_ref()
                .is_none_or(|handle| handle.join.as_ref().is_none_or(JoinHandle::is_finished));
        self.suspended = false;
        if needs_sync {
            self.sync(app);
        }
    }

    pub fn set_phrase(&mut self, app: &AppHandle, phrase: String) {
        let phrase = phrase.trim().to_string();
        if self.phrase == phrase {
            return;
        }
        self.phrase = phrase;
        // Re-arm with the new phrase only if currently listening.
        if self.enabled {
            self.sync(app);
        }
    }

    /// Stop any current listener, then (re)start if enabled + phrase present.
    fn sync(&mut self, app: &AppHandle) {
        if let Some(handle) = self.handle.take() {
            handle.stop();
        }
        if self.enabled && !self.suspended && !self.phrase.is_empty() {
            match start_wake(app.clone(), self.phrase.clone()) {
                Ok(handle) => self.handle = Some(handle),
                Err(error) => {
                    warn!("native wake: {error}");
                    if orb_native_wake_enabled(app) {
                        // Leave the controller lock before lifecycle reactions
                        // suspend/reconfigure the detector.
                        let failure_app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            tokio::task::yield_now().await;
                            crate::orb_window::dispatch(
                                &failure_app,
                                crate::orb_state::OrbAction::RecoverableError { message: error },
                            );
                            crate::orb_window::dispatch(
                                &failure_app,
                                crate::orb_state::OrbAction::Disarm {
                                    reason: crate::orb_state::OrbEndedReason::SessionFailed,
                                },
                            );
                        });
                    }
                },
            }
        }
    }
}

/// Configure the process-owned detector from desktop settings. Unlike the
/// removed webview mirror, this seam is usable before any UI window is visible
/// and therefore survives lock/idle.
pub fn configure_native_wake(app: &AppHandle, enabled: bool, phrase: String) {
    let state = app.state::<crate::AppState>();
    state.orb_wake_enabled.store(enabled, Ordering::Release);
    if let Ok(mut controller) = state.native_wake.lock() {
        controller.set_phrase(app, phrase);
        controller.set_enabled(app, enabled);
    };
}

fn orb_native_wake_enabled(app: &AppHandle) -> bool {
    app.state::<crate::AppState>()
        .orb_wake_enabled
        .load(Ordering::Acquire)
}

pub fn suspend_native_wake(app: &AppHandle) {
    let state = app.state::<crate::AppState>();
    if let Ok(mut controller) = state.native_wake.lock() {
        controller.suspend();
    };
}

pub fn resume_native_wake(app: &AppHandle) {
    let state = app.state::<crate::AppState>();
    if let Ok(mut controller) = state.native_wake.lock() {
        controller.resume(app);
    };
}

// ─── Engine: compiled only with the `native-wake` feature ────────────────────

#[cfg(not(feature = "native-wake"))]
fn start_wake(_app: AppHandle, _phrase: String) -> Result<WakeHandle, String> {
    // Built without `native-wake` — no Vosk/cpal, no libvosk. No-op handle so the
    // controller stays a clean state machine (Orb wake is unavailable in this build).
    Ok(WakeHandle {
        stop: Arc::new(AtomicBool::new(true)),
        join: None,
    })
}

/// Env override for the unpacked Vosk model directory.
#[cfg(feature = "native-wake")]
pub const VOSK_MODEL_DIR_ENV: &str = "MAGICIAN_VOSK_MODEL_DIR";
/// Ignore repeat matches within this window (Vosk re-emits the same partial).
#[cfg(feature = "native-wake")]
const WAKE_FIRE_COOLDOWN_MS: u64 = 4000;
/// Start the process-owned Orb wake detector.
#[cfg(feature = "native-wake")]
fn start_wake(app: AppHandle, phrase: String) -> Result<WakeHandle, String> {
    let model_dir = resolve_vosk_model_dir(&app);
    if !is_complete_vosk_model(std::path::Path::new(&model_dir)) {
        return Err(format!(
            "A complete Vosk wake-word model was not found at '{model_dir}'. Run \
             `make setup-desktop-vosk` or set {VOSK_MODEL_DIR_ENV}."
        ));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let join = std::thread::Builder::new()
        .name("magician-native-wake".to_string())
        .spawn(move || run_wake_loop(app, model_dir, phrase, stop_thread))
        .map_err(|error| format!("failed to spawn wake thread: {error}"))?;
    Ok(WakeHandle {
        stop,
        join: Some(join),
    })
}

/// Resolve the unpacked Vosk model directory:
///   1. `MAGICIAN_VOSK_MODEL_DIR` (dev override)
///   2. `<MAGICIAN_ROOT_DIR>/vosk-model` (or `MAGICIAN_STORAGE_PATH`, then
///      `$HOME/MagicianNotes`)
///   3. the bundled app resources (`Contents/Resources/vosk-model`)
///   4. `<exe dir>/vosk-model`
///
/// Incomplete candidates are skipped so the empty placeholders created by
/// `build.rs` can never shadow a usable runtime or bundled model.
#[cfg(feature = "native-wake")]
fn resolve_vosk_model_dir(app: &AppHandle) -> String {
    if let Ok(dir) = std::env::var(VOSK_MODEL_DIR_ENV) {
        if !dir.trim().is_empty() {
            return dir;
        }
    }

    let runtime_model = if crate::engine_roots::is_remote_engine_process() {
        None
    } else {
        Some(crate::runtime_paths::vosk_model_dir())
    };
    let bundled_model = app
        .path()
        .resource_dir()
        .ok()
        .map(|resources| resources.join(crate::runtime_paths::VOSK_MODEL_DIR_NAME));
    let executable_model = std::env::current_exe().ok().and_then(|exe| {
        exe.parent()
            .map(|dir| dir.join(crate::runtime_paths::VOSK_MODEL_DIR_NAME))
    });

    for candidate in [
        runtime_model.as_ref(),
        bundled_model.as_ref(),
        executable_model.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if is_complete_vosk_model(candidate) {
            return candidate.to_string_lossy().into_owned();
        }
    }

    // Prefer the canonical runtime location in the actionable error message,
    // even before first-time setup has created it.
    runtime_model
        .or(bundled_model)
        .or(executable_model)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "vosk-model".to_string())
}

/// Vosk accepts a directory and otherwise reports only a generic load failure.
/// Check stable files from the published small-model layout first so an empty
/// packaging placeholder is diagnosed before the capture thread starts.
#[cfg(feature = "native-wake")]
fn is_complete_vosk_model(path: &std::path::Path) -> bool {
    path.join("am/final.mdl").is_file() && path.join("conf/model.conf").is_file()
}

/// The wake thread: load the model, open the mic, feed Vosk, fire on the phrase.
/// Everything `!Send` (model, recognizer, cpal stream) is created and dropped
/// here so nothing crosses a thread boundary.
#[cfg(feature = "native-wake")]
fn run_wake_loop(app: AppHandle, model_dir: String, phrase: String, stop: Arc<AtomicBool>) {
    use std::time::{Duration, Instant};

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use tracing::info;

    let Some(model) = vosk::Model::new(model_dir.as_str()) else {
        report_wake_failure(
            &app,
            &stop,
            format!("Failed to load the wake-word model at '{model_dir}'."),
            crate::orb_state::OrbEndedReason::SessionFailed,
        );
        return;
    };

    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        report_wake_failure(
            &app,
            &stop,
            "No default microphone is available.".to_string(),
            crate::orb_state::OrbEndedReason::MicrophoneLost,
        );
        return;
    };
    let supported_config = match device.default_input_config() {
        Ok(config) => config,
        Err(error) => {
            report_wake_failure(
                &app,
                &stop,
                format!("Failed to read the wake microphone: {error}"),
                crate::orb_state::OrbEndedReason::MicrophoneLost,
            );
            return;
        },
    };
    let sample_format = supported_config.sample_format();
    let config: cpal::StreamConfig = supported_config.into();
    // Vosk resamples internally — feed it the device rate directly (the browser
    // passes `audioContext.sampleRate` the same way).
    let sample_rate = config.sample_rate.0 as f32;
    let channels = config.channels.max(1) as usize;

    let needle = normalize_wake_text(&phrase);
    if needle.is_empty() {
        report_wake_failure(
            &app,
            &stop,
            "The configured wake phrase contains no recognizable words.".to_string(),
            crate::orb_state::OrbEndedReason::SessionFailed,
        );
        return;
    }
    // Wake detection is a closed-vocabulary problem. Biasing Vosk to the one
    // active phrase makes short invocations materially more reliable and less
    // CPU-intensive than decoding unrestricted English, while `[unk]` keeps
    // unrelated room speech from being forced into a false match. The shipped
    // small model has the dynamic HCL/grammar layout; retain full recognition
    // as a compatibility fallback for an operator-supplied static-graph model.
    let grammar = [needle.as_str(), "[unk]"];
    let Some(mut recognizer) = vosk::Recognizer::new_with_grammar(
        &model,
        sample_rate,
        &grammar,
    )
    .or_else(|| {
        warn!("native wake: configured Vosk model rejected phrase grammar; using unrestricted recognition");
        vosk::Recognizer::new(&model, sample_rate)
    }) else {
        report_wake_failure(
            &app,
            &stop,
            "Failed to initialize wake-word recognition.".to_string(),
            crate::orb_state::OrbEndedReason::SessionFailed,
        );
        return;
    };

    let (tx, rx) = std::sync::mpsc::channel::<Vec<i16>>();
    let (stream_error_tx, stream_error_rx) = std::sync::mpsc::channel::<String>();
    let stream = match build_wake_stream(
        &device,
        &config,
        sample_format,
        channels,
        tx,
        stream_error_tx,
    ) {
        Ok(stream) => stream,
        Err(error) => {
            report_wake_failure(
                &app,
                &stop,
                error,
                crate::orb_state::OrbEndedReason::MicrophoneLost,
            );
            return;
        },
    };
    if let Err(error) = stream.play() {
        report_wake_failure(
            &app,
            &stop,
            format!("Failed to start the wake microphone: {error}"),
            crate::orb_state::OrbEndedReason::MicrophoneLost,
        );
        return;
    }

    let cooldown = Duration::from_millis(WAKE_FIRE_COOLDOWN_MS);
    let mut last_fire: Option<Instant> = None;
    let mut orb_wake_phrase: Option<String> = None;
    let mut wake_failure: Option<String> = None;
    info!(
        "native wake spotter armed for \"{phrase}\" @ {} Hz / {} ch; conversation capture is idle",
        sample_rate as u32, channels
    );

    while !stop.load(Ordering::Relaxed) {
        if let Ok(error) = stream_error_rx.try_recv() {
            wake_failure = Some(error);
            break;
        }
        let samples = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(samples) => samples,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if !stop.load(Ordering::Relaxed) {
                    wake_failure = Some(stream_error_rx.try_recv().unwrap_or_else(|_| {
                        "The wake microphone stopped delivering audio.".to_string()
                    }));
                }
                break;
            },
        };
        let (heard, finalized) = match recognizer.accept_waveform(&samples) {
            Ok(vosk::DecodingState::Finalized) => match recognizer.result() {
                vosk::CompleteResult::Single(single) => (single.text.to_string(), true),
                vosk::CompleteResult::Multiple(_) => (String::new(), true),
            },
            Ok(vosk::DecodingState::Running) => {
                (recognizer.partial_result().partial.to_string(), false)
            },
            _ => (String::new(), false),
        };
        // A phrase-biased recognizer can transiently project its only known
        // phrase into a partial result while the microphone settles or hears
        // unrelated speech. Partials are useful diagnostics, never authority
        // to open conversation capture. Require a finalized utterance that
        // starts with the configured invocation (an immediately following
        // request is allowed) before handing microphone ownership over.
        if !finalized_wake_phrase_matches(finalized, &heard, &needle) {
            continue;
        }
        let now = Instant::now();
        let ready = last_fire
            .map(|prev| now.duration_since(prev) >= cooldown)
            .unwrap_or(true);
        if ready {
            last_fire = Some(now);
            info!("native wake phrase accepted (heard: \"{heard}\")");
            // Clear so the same utterance's lingering partials don't re-match.
            recognizer.reset();
            if orb_native_wake_enabled(&app) {
                orb_wake_phrase = Some(phrase.clone());
                // Release the microphone before the realtime conversation opens
                // its own capture stream. The controller remains logically
                // enabled and is resumed by the lifecycle after the exchange.
                break;
            }
        }
    }

    drop(stream);
    if let Some(error) = wake_failure {
        report_wake_failure(
            &app,
            &stop,
            error,
            crate::orb_state::OrbEndedReason::MicrophoneLost,
        );
    } else if let Some(phrase) = orb_wake_phrase {
        // Dispatch after this capture thread has returned. The transition
        // suspends the detector and joins its WakeHandle; joining ourselves
        // here would deadlock. A short async handoff also guarantees cpal has
        // released the input device before realtime capture starts.
        let wake_app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            crate::orb_window::accept_native_wake(wake_app, phrase).await;
        });
    }
    info!("native wake stopped");
}

/// Normalize configured and decoded wake text through the same boundary.
/// Punctuation and repeated whitespace must not turn an otherwise exact spoken
/// phrase into a miss; alphanumeric Unicode aliases remain intact.
#[cfg(feature = "native-wake")]
fn normalize_wake_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in value.trim().chars() {
        if character.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.extend(character.to_lowercase());
            pending_space = false;
        } else if !normalized.is_empty() {
            pending_space = true;
        }
    }
    normalized
}

/// The full conversation rail may start only from a completed invocation.
/// Requiring the phrase at the utterance boundary prevents both partial-result
/// startup hallucinations and incidental mid-sentence mentions from acting as
/// wake commands, while still allowing "hey assistant, help me".
#[cfg(feature = "native-wake")]
fn finalized_wake_phrase_matches(finalized: bool, heard: &str, needle: &str) -> bool {
    if !finalized {
        return false;
    }
    let heard = normalize_wake_text(heard);
    let needle = normalize_wake_text(needle);
    if heard.is_empty() || needle.is_empty() {
        return false;
    }
    heard == needle
        || heard
            .strip_prefix(&needle)
            .is_some_and(|remainder| remainder.starts_with(' '))
}

#[cfg(feature = "native-wake")]
fn report_wake_failure(
    app: &AppHandle,
    stop: &AtomicBool,
    message: String,
    reason: crate::orb_state::OrbEndedReason,
) {
    warn!("native wake: {message}");
    if stop.load(Ordering::Relaxed) || !orb_native_wake_enabled(app) {
        return;
    }
    crate::orb_window::dispatch(
        app,
        crate::orb_state::OrbAction::RecoverableError { message },
    );
    crate::orb_window::dispatch(app, crate::orb_state::OrbAction::Disarm { reason });
}

/// Build a mic input stream whose callback converts to mono PCM16 (reusing the
/// voice-note converters) and forwards it to the wake thread.
#[cfg(feature = "native-wake")]
fn build_wake_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: cpal::SampleFormat,
    channels: usize,
    tx: std::sync::mpsc::Sender<Vec<i16>>,
    stream_error_tx: std::sync::mpsc::Sender<String>,
) -> Result<cpal::Stream, String> {
    use cpal::traits::DeviceTrait;

    use crate::voice_note::{
        pcm_f32_to_mono_i16_samples, pcm_i16_to_mono_i16_samples, pcm_u16_to_mono_i16_samples,
    };

    let built = match format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mono = pcm_f32_to_mono_i16_samples(data, channels);
                if !mono.is_empty() {
                    let _ = tx.send(mono);
                }
            },
            move |error| {
                let _ = stream_error_tx.send(format!("Wake microphone stream failed: {error}"));
            },
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let mono = pcm_i16_to_mono_i16_samples(data, channels);
                if !mono.is_empty() {
                    let _ = tx.send(mono);
                }
            },
            move |error| {
                let _ = stream_error_tx.send(format!("Wake microphone stream failed: {error}"));
            },
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let mono = pcm_u16_to_mono_i16_samples(data, channels);
                if !mono.is_empty() {
                    let _ = tx.send(mono);
                }
            },
            move |error| {
                let _ = stream_error_tx.send(format!("Wake microphone stream failed: {error}"));
            },
            None,
        ),
        other => return Err(format!("unsupported mic sample format for wake: {other:?}")),
    };
    built.map_err(|error| format!("failed to build wake input stream: {error}"))
}

#[cfg(all(test, feature = "native-wake"))]
mod tests {
    use super::{finalized_wake_phrase_matches, is_complete_vosk_model, normalize_wake_text};

    #[test]
    fn wake_text_normalization_matches_spoken_decoder_shape() {
        assert_eq!(normalize_wake_text("  Hey,  Assistant! "), "hey assistant");
        assert_eq!(normalize_wake_text("Ask-Sam"), "ask sam");
        assert_eq!(normalize_wake_text("!!!"), "");
    }

    #[test]
    fn wake_gate_rejects_partials_and_requires_a_phrase_led_final_utterance() {
        assert!(!finalized_wake_phrase_matches(
            false,
            "hey assistant",
            "hey assistant",
        ));
        assert!(!finalized_wake_phrase_matches(
            true,
            "I said hey assistant yesterday",
            "hey assistant",
        ));
        assert!(!finalized_wake_phrase_matches(
            true,
            "hey assistantship",
            "hey assistant",
        ));
        assert!(finalized_wake_phrase_matches(
            true,
            "Hey, assistant!",
            "hey assistant",
        ));
        assert!(finalized_wake_phrase_matches(
            true,
            "hey assistant help me plan today",
            "hey assistant",
        ));
    }

    #[test]
    fn empty_packaging_placeholder_is_not_a_complete_vosk_model() {
        let root = unique_test_dir("empty");
        std::fs::create_dir_all(&root).expect("create placeholder");

        assert!(!is_complete_vosk_model(&root));

        std::fs::remove_dir_all(root).expect("remove placeholder");
    }

    #[test]
    fn published_small_model_layout_is_recognized() {
        let root = unique_test_dir("complete");
        std::fs::create_dir_all(root.join("am")).expect("create am");
        std::fs::create_dir_all(root.join("conf")).expect("create conf");
        std::fs::write(root.join("am/final.mdl"), b"model").expect("write model");
        std::fs::write(root.join("conf/model.conf"), b"config").expect("write config");

        assert!(is_complete_vosk_model(&root));

        std::fs::remove_dir_all(root).expect("remove model");
    }

    fn unique_test_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "magican-vosk-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ))
    }
}
