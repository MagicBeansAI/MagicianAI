use crate::config::{save_config as save_config_to_disk, MagicianDesktopConfig};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures_util::{SinkExt, StreamExt};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    collections::VecDeque,
    fs,
    io::{BufWriter, Cursor},
    path::PathBuf,
    sync::mpsc,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

const VOICE_NOTE_EVENT: &str = "voice-note-state";
const VOICE_NOTE_SOURCE_SURFACE: &str = "global_voice_note";
const AMBIENT_DICTATION_SOURCE_SURFACE: &str = "desktop_ambient_dictation";
const VOICE_NOTE_MIME_TYPE: &str = "audio/wav";
const VOICE_NOTE_FILENAME: &str = "voice-note.wav";
const VOICE_NOTE_HTTP_TIMEOUT: Duration = Duration::from_secs(120);
const VOICE_NOTE_EVENT_HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const VOICE_NOTE_MAX_DURATION: Duration = Duration::from_secs(120);
const VOICE_NOTE_TEST_RECORD_DURATION: Duration = Duration::from_secs(3);
const VOICE_NOTE_TEST_PLAYBACK_PAD: Duration = Duration::from_millis(250);
const MEDIA_VOICE_NOTE_RECORDING_STARTED: &str = "media.voice_note.recording_started";
const MEDIA_VOICE_NOTE_RECORDING_STOPPED: &str = "media.voice_note.recording_stopped";
const MEDIA_VOICE_NOTE_RECORDING_FAILED: &str = "media.voice_note.recording_failed";
const LIVE_PTT_SOURCE_SURFACE: &str = "global_live_ptt";
/// Push-to-talk modes for the single mode-aware Left-Option hold, stored in
/// `AppState::ptt_mode` and mirrored from the web's universal Call/Dictate
/// switch. `LIVE` engages a realtime voice turn; `DICTATE` records a dictation
/// take.
pub const PTT_MODE_LIVE: u8 = 0;
pub const PTT_MODE_DICTATE: u8 = 1;

/// Map a web/store `voice_mode` string (`realtime`/`hands_free`/`recording` + aliases) to the
/// native PTT-mode constant. Anything unrecognized or empty resolves to
/// `DICTATE`, never `LIVE`: an absent/garbled value must NOT silently arm a live
/// mic. This is the single mapping shared by the startup seed and the media-
/// preference apply path so they can never disagree on what a value means.
pub fn ptt_mode_from_voice_mode(mode: &str) -> u8 {
    match mode.trim().to_ascii_lowercase().as_str() {
        "realtime" | "live" | "call" | "hands_free" | "handsfree" => PTT_MODE_LIVE,
        _ => PTT_MODE_DICTATE,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrbVoiceMode {
    Dictation,
    HandsFree,
    Realtime,
}

/// Resolve the Ambient Orb's independent three-way choice. Dictation is a
/// native bounded turn loop and is never sent to the streaming voice socket.
fn configured_orb_voice_mode(mode: &str) -> OrbVoiceMode {
    match crate::config::normalize_orb_voice_mode(mode).as_str() {
        "dictation" => OrbVoiceMode::Dictation,
        "realtime" => OrbVoiceMode::Realtime,
        _ => OrbVoiceMode::HandsFree,
    }
}

fn live_ptt_wire_voice_mode(
    configured_voice_mode: &str,
    admitted_orb_mode: Option<OrbVoiceMode>,
) -> Result<String, String> {
    match admitted_orb_mode {
        Some(mode) => mode
            .streaming_wire_value()
            .map(str::to_string)
            .ok_or_else(|| "ambient Dictation cannot use the streaming transport".to_string()),
        None => Ok(configured_voice_mode.to_string()),
    }
}

/// Wake-word and Talk Now Live sessions still need a continuous server-VAD
/// boundary, because nothing will send `ptt.release`. A Left Option hold is
/// press-to-talk: omit the override so the shared profile's manual boundary
/// commits on release, and the socket can stay up between holds.
fn live_ptt_turn_boundary(
    admitted_orb_mode: Option<OrbVoiceMode>,
    press_to_talk: bool,
) -> Option<&'static str> {
    if press_to_talk {
        None
    } else {
        matches!(admitted_orb_mode, Some(OrbVoiceMode::Realtime)).then_some("server_vad")
    }
}

fn should_fallback_orb_realtime(
    failed: bool,
    admitted_orb_mode: Option<OrbVoiceMode>,
    still_owned: bool,
) -> bool {
    failed && matches!(admitted_orb_mode, Some(OrbVoiceMode::Realtime)) && still_owned
}

impl OrbVoiceMode {
    fn streaming_wire_value(self) -> Option<&'static str> {
        match self {
            Self::Dictation => None,
            Self::HandsFree => Some("hands_free"),
            Self::Realtime => Some("realtime"),
        }
    }
}

const LIVE_PTT_HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const LIVE_PTT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);
const LIVE_PTT_PLAYBACK_PREBUFFER_MS: u64 = 180;
const LIVE_PTT_PLAYBACK_MAX_BUFFER_MS: u64 = 30_000;
const LIVE_PTT_PLAYBACK_PRODUCER_HIGH_WATER_MS: u64 = 600;
const LIVE_PTT_PLAYBACK_PRODUCER_LOW_WATER_MS: u64 = 350;
/// Raw CPAL capture has no acoustic echo canceller. Keep cascaded Hands-free
/// half-duplex until the native playback queue drains, then allow a short
/// room/speaker tail to decay before forwarding microphone PCM again.
const LIVE_PTT_HANDS_FREE_ECHO_TAIL: Duration = Duration::from_millis(400);
const LIVE_PTT_AUDIO_QUEUE_CAPACITY: usize = 128;
const LIVE_PTT_CONTROL_QUEUE_CAPACITY: usize = 32;
const LIVE_PTT_CONTROL_SEND_TIMEOUT: Duration = Duration::from_secs(1);
const LIVE_PTT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const LIVE_PTT_WIRE_SAMPLE_RATE_HZ: u32 = 24_000;
const MEDIA_VOICE_BRIDGE_CONNECTED: &str = "media.voice.bridge.connected";
const MEDIA_VOICE_BRIDGE_DISCONNECTED: &str = "media.voice.bridge.disconnected";
const MEDIA_VOICE_CLIENT_AUDIO: &str = "media.voice.client_audio";
const MEDIA_VOICE_CONTROLLER_COMMAND: &str = "media.voice.controller_command";
const MEDIA_VOICE_BRIDGE_ERROR: &str = "media.voice.bridge.error";
const ORB_SESSION_NONE: u64 = 0;
const ORB_SESSION_PENDING: u64 = u64::MAX;
const ASSISTANT_FALLBACK_NAME: &str = "Assistant";
const AMBIENT_DICTATION_POLL_INTERVAL: Duration = Duration::from_millis(100);
const AMBIENT_DICTATION_TRAILING_SILENCE: Duration = Duration::from_millis(1_100);
const AMBIENT_DICTATION_MAX_UTTERANCE: Duration = Duration::from_secs(45);
const AMBIENT_DICTATION_MAX_TURN: Duration = Duration::from_secs(180);
/// Floor of the speech threshold, on the input meter's 0..1 scale: the level a
/// quiet room must exceed to count as speech. The live threshold sits above the
/// measured noise floor (see [`AmbientDictationSilenceGate`]); this is where it
/// bottoms out.
const AMBIENT_DICTATION_SPEECH_LEVEL: f32 = 0.012;
/// Speech must be this many times the room's noise floor. About 8 dB, the
/// usual margin for an energy gate: a fixed line at `SPEECH_LEVEL` alone kept
/// the gate open on ordinary room noise, so a four-second question waited 35 s
/// for a quiet dip and an empty room ran the whole 45 s cap as "speech".
const AMBIENT_DICTATION_SPEECH_OVER_FLOOR: f32 = 2.5;
/// Ceiling of the speech threshold, so a loud room can still be spoken over.
const AMBIENT_DICTATION_SPEECH_LEVEL_MAX: f32 = 0.25;
/// The first ticks after the wake phrase measure the room before anything can
/// count as speech; the floor adapts fast here and slowly afterwards.
const AMBIENT_DICTATION_FLOOR_CALIBRATION: Duration = Duration::from_millis(300);
const AMBIENT_DICTATION_FLOOR_CALIBRATION_RATE: f32 = 0.5;
const AMBIENT_DICTATION_FLOOR_RISE_RATE: f32 = 0.05;
const AMBIENT_DICTATION_FLOOR_FALL_RATE: f32 = 0.5;
/// While the talker is speaking, a tick this far below their running level is
/// a lull — the room, not the voice — and may feed the floor. Without it, an
/// eager talker who starts before calibration in a noisy room would leave the
/// floor at zero and the room noise would keep the turn open to the cap.
const AMBIENT_DICTATION_SPEECH_GAP_RATIO: f32 = 0.35;
const AMBIENT_DICTATION_SPEECH_LEVEL_RATE: f32 = 0.2;

fn claim_orb_session(owner: &AtomicU64, sequence: u64) -> bool {
    owner
        .compare_exchange(
            ORB_SESSION_PENDING,
            sequence,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

fn release_orb_session(owner: &AtomicU64, sequence: u64) -> bool {
    owner
        .compare_exchange(
            sequence,
            ORB_SESSION_NONE,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

fn release_external_voice_owner(owner: &AtomicU64, token: u64) -> bool {
    owner
        .compare_exchange(token, ORB_SESSION_NONE, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

fn next_orb_sequence(current: u64) -> u64 {
    let next = current.wrapping_add(1);
    if matches!(next, ORB_SESSION_NONE | ORB_SESSION_PENDING) {
        1
    } else {
        next
    }
}

fn assistant_caption_final(kind: &str) -> Option<bool> {
    match kind {
        "transcript.assistant" => Some(true),
        "transcript.assistant.delta" => Some(false),
        _ => None,
    }
}

type SharedWavWriter = Arc<StdMutex<Option<WavWriter<BufWriter<fs::File>>>>>;
type SharedPlaybackQueue = Arc<StdMutex<PlaybackQueueState>>;

#[derive(Default)]
pub struct VoiceHotkeyState {
    pub registered_voice_note_shortcut: Option<String>,
    pub registered_live_ptt_shortcut: Option<String>,
    pub note: VoiceNoteCaptureState,
    pub live: LivePttState,
}

#[derive(Default)]
pub struct LivePttState {
    active: Option<ActiveLivePtt>,
    capture: Option<LivePttCapture>,
    starting: bool,
    muted: bool,
    counter: u64,
    idle_generation: u64,
    input_speech_active: bool,
    active_output_response_id: Option<String>,
}

#[derive(Default)]
pub struct VoiceNoteCaptureState {
    active: Option<ActiveVoiceNote>,
    starting: bool,
    stop_after_start: bool,
    counter: u64,
}

struct ActiveVoiceNote {
    stop_tx: mpsc::Sender<()>,
    join_handle: std::thread::JoinHandle<Result<CompletedVoiceNote, String>>,
    started_at: Instant,
    sequence: u64,
    external_voice_token: Option<u64>,
}

struct AmbientDictationCapture {
    level_meter: Arc<AtomicU32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AmbientDictationBoundary {
    SpeechComplete,
    NoSpeech,
    /// The hold ended before any speech. Stop the microphone and wait.
    HoldIdle,
    Cancelled,
}

/// End-of-speech detector for an Orb dictation turn, fed one input-meter
/// reading per poll tick.
///
/// The threshold is relative to the room: an estimate of the noise floor is
/// kept from ticks that are not speech (fast to fall, slow to rise, and never
/// raised by speech itself), and a tick counts as speech only when it clears
/// `AMBIENT_DICTATION_SPEECH_OVER_FLOOR` times that floor, bounded below by
/// `AMBIENT_DICTATION_SPEECH_LEVEL` and above by `…_SPEECH_LEVEL_MAX`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct AmbientDictationSilenceGate {
    heard_speech: bool,
    last_speech_at: Option<Duration>,
    no_speech_after: Duration,
    /// Estimate of the room's level while nobody is talking.
    noise_floor: f32,
    /// Running level of the talker while they are talking.
    speech_level: f32,
}

impl AmbientDictationSilenceGate {
    fn new(no_speech_after: Duration) -> Self {
        Self {
            heard_speech: false,
            last_speech_at: None,
            no_speech_after,
            noise_floor: 0.0,
            speech_level: 0.0,
        }
    }

    fn speech_threshold(&self) -> f32 {
        (self.noise_floor * AMBIENT_DICTATION_SPEECH_OVER_FLOOR).clamp(
            AMBIENT_DICTATION_SPEECH_LEVEL,
            AMBIENT_DICTATION_SPEECH_LEVEL_MAX,
        )
    }

    fn adapt_floor(&mut self, level: f32, rate: f32) {
        self.noise_floor += (level - self.noise_floor) * rate;
    }

    fn observe(&mut self, level: f32, elapsed: Duration) -> Option<AmbientDictationBoundary> {
        let calibrating = elapsed < AMBIENT_DICTATION_FLOOR_CALIBRATION;
        // A level the threshold ceiling could never treat as speech is the
        // room; anything louder during calibration is an eager talker and
        // must not be mistaken for the floor.
        let plausibly_room =
            level < AMBIENT_DICTATION_SPEECH_LEVEL_MAX / AMBIENT_DICTATION_SPEECH_OVER_FLOOR;
        let is_speech = !calibrating && level >= self.speech_threshold();
        let lull = is_speech && level < self.speech_level * AMBIENT_DICTATION_SPEECH_GAP_RATIO;
        if calibrating {
            if plausibly_room {
                self.adapt_floor(level, AMBIENT_DICTATION_FLOOR_CALIBRATION_RATE);
            }
        } else if !is_speech {
            let rate = if level < self.noise_floor {
                AMBIENT_DICTATION_FLOOR_FALL_RATE
            } else {
                AMBIENT_DICTATION_FLOOR_RISE_RATE
            };
            self.adapt_floor(level, rate);
        } else if lull {
            self.adapt_floor(level, AMBIENT_DICTATION_FLOOR_RISE_RATE);
        }
        if is_speech {
            // A lull is the room, so it must not drag the talker's level down
            // to meet it — that would hide the lull again.
            if !lull {
                self.speech_level = if self.speech_level <= 0.0 {
                    level
                } else {
                    self.speech_level
                        + (level - self.speech_level) * AMBIENT_DICTATION_SPEECH_LEVEL_RATE
                };
            }
            self.last_speech_at = Some(elapsed);
            self.heard_speech = true;
        }
        if self.heard_speech {
            if elapsed >= AMBIENT_DICTATION_MAX_UTTERANCE
                || self.last_speech_at.is_some_and(|last| {
                    elapsed.saturating_sub(last) >= AMBIENT_DICTATION_TRAILING_SILENCE
                })
            {
                return Some(AmbientDictationBoundary::SpeechComplete);
            }
            return None;
        }
        (elapsed >= self.no_speech_after).then_some(AmbientDictationBoundary::NoSpeech)
    }

    fn hold_release_boundary(self) -> AmbientDictationBoundary {
        if self.heard_speech {
            AmbientDictationBoundary::SpeechComplete
        } else {
            AmbientDictationBoundary::HoldIdle
        }
    }
}

struct WavRecorder {
    stream: cpal::Stream,
    writer: SharedWavWriter,
    path: PathBuf,
    samples_written: Arc<AtomicU64>,
    input_label: String,
}

struct CompletedVoiceNote {
    bytes: Vec<u8>,
    samples_written: u64,
    input_label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceNoteRecordingTestResult {
    pub duration_ms: u128,
    pub audio_bytes: usize,
    pub samples_written: u64,
    pub input_label: String,
    pub playback_ok: bool,
    pub playback_duration_ms: Option<u128>,
    pub playback_error: Option<String>,
    pub transcript: Option<String>,
    pub stt_model: Option<String>,
    pub language: Option<String>,
    pub transcription_error: Option<String>,
}

struct ActiveLivePtt {
    audio_tx: tokio_mpsc::Sender<Vec<u8>>,
    control_tx: tokio_mpsc::Sender<LivePttControlCommand>,
    session_id: String,
    sequence: u64,
    external_voice_token: Option<u64>,
    /// Set from the backend's scope-authoritative `session.ready.agent`
    /// identity before assistant transcripts can arrive.
    assistant_name: Option<String>,
    /// Live press-to-talk keeps this socket after the microphone closes.
    retain_connection: bool,
}

struct LivePttCapture {
    stop_tx: mpsc::Sender<()>,
    samples_sent: Arc<AtomicU64>,
    input_label: String,
}

impl Drop for LivePttCapture {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
    }
}

struct LivePttCaptureReady {
    samples_sent: Arc<AtomicU64>,
    input_label: String,
}

struct LivePttInputStream {
    _stream: cpal::Stream,
    samples_sent: Arc<AtomicU64>,
    input_label: String,
}

#[derive(Debug)]
struct HalfDuplexInputGate {
    enabled: bool,
    resume_at: Option<Instant>,
    playback_was_active: bool,
}

impl HalfDuplexInputGate {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            resume_at: None,
            playback_was_active: false,
        }
    }

    fn note_output_started(&mut self, now: Instant) {
        if self.enabled {
            self.playback_was_active = true;
            self.resume_at = Some(now + LIVE_PTT_HANDS_FREE_ECHO_TAIL);
        }
    }

    fn allows_input(&mut self, now: Instant, playback_active: bool, output_muted: bool) -> bool {
        if !self.enabled || output_muted {
            self.resume_at = None;
            self.playback_was_active = false;
            return true;
        }
        if playback_active {
            self.playback_was_active = true;
            self.resume_at = Some(now + LIVE_PTT_HANDS_FREE_ECHO_TAIL);
            return false;
        }
        if self.playback_was_active {
            self.playback_was_active = false;
            self.resume_at = Some(now + LIVE_PTT_HANDS_FREE_ECHO_TAIL);
        }
        match self.resume_at {
            Some(resume_at) if now < resume_at => false,
            Some(_) => {
                self.resume_at = None;
                true
            },
            None => true,
        }
    }
}

enum LivePttControlCommand {
    Engage,
    Release,
    ClearInput,
    Interrupt,
    SetOutputMuted(bool),
    End,
}

impl LivePttControlCommand {
    fn label(&self) -> &'static str {
        match self {
            Self::Engage => "ptt.engage",
            Self::Release => "ptt.release",
            Self::ClearInput => "input.clear",
            Self::Interrupt => "response.interrupt",
            Self::SetOutputMuted(true) => "output.mute",
            Self::SetOutputMuted(false) => "output.unmute",
            Self::End => "session.end",
        }
    }
}

async fn send_live_ptt_control(
    tx: &tokio_mpsc::Sender<LivePttControlCommand>,
    command: LivePttControlCommand,
) -> Result<(), String> {
    let label = command.label();
    match tokio::time::timeout(LIVE_PTT_CONTROL_SEND_TIMEOUT, tx.send(command)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => {
            let message = format!("live PTT control loop closed before {label}");
            warn!("{}", message);
            Err(message)
        },
        Err(_) => {
            let message = format!("timed out queueing live PTT control command {label}");
            warn!("{}", message);
            Err(message)
        },
    }
}

#[derive(Debug, Deserialize)]
struct MediaSessionEnvelope {
    session: MediaSessionWire,
}

#[derive(Debug, Deserialize)]
struct MediaSessionWire {
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct VoiceControlEnvelope {
    kind: String,
    #[serde(default)]
    payload: Value,
}

struct LivePttControlOutcome {
    keep_open: bool,
    clear_playback: bool,
    begin_playback_segment: bool,
    end_playback_segment: bool,
}

impl Default for LivePttControlOutcome {
    fn default() -> Self {
        Self {
            keep_open: true,
            clear_playback: false,
            begin_playback_segment: false,
            end_playback_segment: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct VoiceNoteStateEvent<'a> {
    state: &'a str,
    message: &'a str,
    sequence: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct VoiceNoteSubmitResponse {
    chat_session_id: String,
    #[serde(default)]
    chat_turn_id: Option<String>,
    transcript: String,
    #[serde(default)]
    assistant_preview: Option<String>,
    #[serde(default)]
    assistant_speech_segments: Option<Vec<VoiceSpeechSegment>>,
    #[serde(default)]
    queued_message_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct VoiceNoteBackendErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    chat_session_id: Option<String>,
}

#[derive(Debug, Clone)]
struct VoiceNoteBackendRejection {
    status: reqwest::StatusCode,
    code: Option<String>,
    message: Option<String>,
    chat_session_id: Option<String>,
    raw_body: String,
}

impl VoiceNoteBackendRejection {
    fn from_response(status: reqwest::StatusCode, raw_body: String) -> Self {
        let parsed = serde_json::from_str::<VoiceNoteBackendErrorBody>(&raw_body).ok();
        Self {
            status,
            code: parsed.as_ref().and_then(|body| body.error.clone()),
            message: parsed.as_ref().and_then(|body| body.message.clone()),
            chat_session_id: parsed.and_then(|body| body.chat_session_id),
            raw_body,
        }
    }

    fn is_recoverable_ambient_dictation(&self) -> bool {
        matches!(
            self.code.as_deref(),
            Some("unsupported_language") | Some("empty_transcript") | Some("no_speech")
        )
    }

    fn spoken_guidance(&self) -> String {
        self.message
            .as_deref()
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .unwrap_or("I didn’t catch that. Please try again.")
            .chars()
            .take(320)
            .collect()
    }
}

impl std::fmt::Display for VoiceNoteBackendRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(message) = self
            .message
            .as_deref()
            .map(str::trim)
            .filter(|message| !message.is_empty())
        {
            write!(formatter, "The backend returned {}: {message}", self.status)
        } else {
            write!(
                formatter,
                "The backend returned {}: {}",
                self.status, self.raw_body
            )
        }
    }
}

enum VoiceNoteSubmitOutcome {
    Accepted(VoiceNoteSubmitResponse),
    Rejected(VoiceNoteBackendRejection),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct VoiceSpeechSegment {
    text: String,
    #[serde(default)]
    emotion: Option<String>,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    pace: Option<String>,
    #[serde(default)]
    voice_mode: Option<String>,
    #[serde(default)]
    emphasis: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MediaSessionListEnvelope {
    #[serde(default)]
    sessions: Vec<MediaSessionListEntry>,
}

#[derive(Debug, Deserialize)]
struct MediaSessionListEntry {
    #[serde(default)]
    surface_type: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    capabilities: MediaSessionCapabilities,
}

#[derive(Debug, Default, Deserialize)]
struct MediaSessionCapabilities {
    #[serde(default)]
    browser_tts: bool,
    #[serde(default)]
    provider_tts: bool,
    #[serde(default)]
    realtime_voice: bool,
}

#[derive(Debug, Serialize)]
struct MascotTtsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    segments: Vec<VoiceSpeechSegment>,
    format: &'static str,
}

#[derive(Debug, Deserialize)]
struct MascotTtsEnvelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    audio_b64: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

struct MascotTtsAudioSegment {
    audio: Vec<u8>,
    content_type: String,
    provider: Option<String>,
    model: Option<String>,
}

#[tauri::command]
pub async fn toggle_voice_note(app: AppHandle) -> Result<(), String> {
    if is_recording_or_starting(&app).await {
        stop_voice_note(app).await
    } else {
        start_voice_note(app).await
    }
}

#[tauri::command]
pub async fn start_voice_note(app: AppHandle) -> Result<(), String> {
    start_voice_note_capture(app).await
}

#[tauri::command]
pub async fn stop_voice_note(app: AppHandle) -> Result<(), String> {
    stop_voice_note_capture(app).await
}

#[tauri::command]
pub async fn run_voice_note_recording_test(
    app: AppHandle,
) -> Result<VoiceNoteRecordingTestResult, String> {
    {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        if orb_conversation_has_owner(&app)
            || voice.note.starting
            || voice.note.active.is_some()
            || voice.live.starting
            || voice.live.active.is_some()
            || voice.live.capture.is_some()
        {
            return Err(
                "Stop the active voice note or live PTT session before running the recording test."
                    .to_string(),
            );
        }
        voice.note.starting = true;
        voice.note.stop_after_start = false;
    }
    let external_voice_token = match begin_external_voice(&app) {
        Ok(token) => token,
        Err(error) => {
            let state = app.state::<crate::AppState>();
            let mut voice = state.voice_hotkeys.lock().await;
            voice.note.starting = false;
            return Err(error);
        },
    };
    let started = Instant::now();
    let completed_result = match tokio::task::spawn_blocking(|| {
        record_voice_note_for(VOICE_NOTE_TEST_RECORD_DURATION)
    })
    .await
    {
        Ok(result) => result,
        Err(error) => Err(format!("voice note recording test thread failed: {error}")),
    };
    {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        voice.note.starting = false;
        voice.note.stop_after_start = false;
    }
    end_external_voice(&app, external_voice_token);
    let completed = completed_result?;
    let duration_ms = started.elapsed().as_millis();
    let audio_bytes = completed.bytes.len();
    let samples_written = completed.samples_written;
    let input_label = completed.input_label.clone();

    info!(
        duration_ms,
        audio_bytes,
        samples_written,
        input = input_label.as_str(),
        "Captured desktop voice recording test"
    );

    let playback_audio = completed.bytes.clone();
    let playback =
        match tokio::task::spawn_blocking(move || play_voice_test_wav(&playback_audio)).await {
            Ok(result) => result,
            Err(error) => Err(format!(
                "voice recording test playback thread failed: {error}"
            )),
        };
    let (playback_ok, playback_duration_ms, playback_error) = match playback {
        Ok(playback_duration_ms) => (true, Some(playback_duration_ms), None),
        Err(error) => (false, None, Some(error)),
    };

    let transcription = crate::host_gateway::transcribe_speech_audio(
        &app,
        completed.bytes,
        Some(VOICE_NOTE_FILENAME.to_string()),
        VOICE_NOTE_MIME_TYPE.to_string(),
        None,
        Some("settings-recording-test".to_string()),
    )
    .await;
    let (transcript, stt_model, language, transcription_error) = match transcription {
        Ok(response) => (
            Some(response.transcript),
            Some(response.model),
            response.language,
            None,
        ),
        Err(error) => (None, None, None, Some(error)),
    };

    Ok(VoiceNoteRecordingTestResult {
        duration_ms,
        audio_bytes,
        samples_written,
        input_label,
        playback_ok,
        playback_duration_ms,
        playback_error,
        transcript,
        stt_model,
        language,
        transcription_error,
    })
}

#[tauri::command]
pub async fn end_live_ptt(app: AppHandle) -> Result<(), String> {
    end_live_ptt_session(app, "user_requested").await
}

/// Left Option went down long enough to talk. Opens the microphone on a
/// parked session, or admits a new conversation when nothing is connected.
pub fn trigger_orb_hold_start(app: &AppHandle) {
    let state = app.state::<crate::AppState>();
    state.orb_ptt_session.store(true, Ordering::Release);
    state.orb_ptt_down.store(true, Ordering::Release);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = orb_hold_start(app).await {
            warn!("Orb press-to-talk could not start: {error}");
        }
    });
}

/// Left Option came up. Closes the microphone. A Live session keeps its socket.
pub fn trigger_orb_hold_release(app: &AppHandle) {
    app.state::<crate::AppState>()
        .orb_ptt_down
        .store(false, Ordering::Release);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = release_live_ptt(app).await {
            warn!("Orb press-to-talk could not release: {error}");
        }
    });
}

async fn orb_hold_start(app: AppHandle) -> Result<(), String> {
    if app
        .state::<crate::AppState>()
        .orb_setup_blocked
        .load(Ordering::Acquire)
    {
        return Ok(());
    }
    if reengage_orb_listen(&app).await? {
        return Ok(());
    }
    let state = crate::orb_window::current_snapshot(&app).state;
    if matches!(state, "off" | "ended" | "disarming") {
        crate::orb_window::orb_rearm(app.clone()).await?;
    }
    let state = crate::orb_window::current_snapshot(&app).state;
    if !matches!(state, "armed" | "cooldown" | "hold_ready") || orb_conversation_has_owner(&app) {
        return Ok(());
    }
    let before = crate::orb_window::current_snapshot(&app);
    let after = crate::orb_window::dispatch(
        &app,
        crate::orb_state::OrbAction::WakeHeard {
            phrase: "Ready".to_string(),
        },
    );
    if after.revision == before.revision || after.state != "heard" {
        Err(format!(
            "orb cannot start press-to-talk while {}",
            before.state
        ))
    } else {
        Ok(())
    }
}

/// Re-open the microphone on an Orb session that is already connected.
/// Returns true when this hold should not also admit a new conversation.
async fn reengage_orb_listen(app: &AppHandle) -> Result<bool, String> {
    let existing = {
        let state = app.state::<crate::AppState>();
        let voice = state.voice_hotkeys.lock().await;
        let Some(active) = voice.live.active.as_ref() else {
            return Ok(false);
        };
        if !orb_conversation_owns(app, active.sequence) {
            return Ok(false);
        }
        if voice.live.starting || voice.live.capture.is_some() {
            return Ok(true);
        }
        Some((
            active.audio_tx.clone(),
            active.control_tx.clone(),
            active.sequence,
        ))
    };
    let Some((audio_tx, control_tx, sequence)) = existing else {
        return Ok(false);
    };
    if let Err(error) = send_live_ptt_control(&control_tx, LivePttControlCommand::Engage).await {
        return Err(error);
    }
    let capture_result = {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        let still_active = voice
            .live
            .active
            .as_ref()
            .is_some_and(|active| active.sequence == sequence);
        if !still_active || voice.live.capture.is_some() {
            Err("live voice ended while the microphone was opening".to_string())
        } else {
            match LivePttCapture::start(app.clone(), audio_tx, sequence) {
                Ok(capture) => {
                    voice.live.capture = Some(capture);
                    Ok(())
                },
                Err(error) => Err(format!("Live PTT could not access the microphone: {error}")),
            }
        }
    };
    capture_result?;
    if !app
        .state::<crate::AppState>()
        .orb_ptt_down
        .load(Ordering::Acquire)
    {
        release_live_ptt(app.clone()).await?;
        return Ok(true);
    }
    crate::orb_window::dispatch(
        app,
        crate::orb_state::OrbAction::Turn(crate::orb_state::OrbTurn::Listening),
    );
    let _ = set_voice_visual(app, "listening").await;
    Ok(true)
}

/// Start the ambient orb's independently configured conversation transport.
/// Streaming modes keep a voice socket open; Dictation runs bounded native
/// record -> STT/agent -> TTS turns. Neither depends on an open browser tab.
pub async fn begin_orb_conversation(app: AppHandle) -> Result<(), String> {
    app.state::<crate::AppState>()
        .orb_conversation_owner
        .compare_exchange(
            ORB_SESSION_NONE,
            ORB_SESSION_PENDING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| "another orb conversation is already active".to_string())?;
    let busy = {
        let state = app.state::<crate::AppState>();
        let voice = state.voice_hotkeys.lock().await;
        voice.note.starting
            || voice.note.active.is_some()
            || voice.live.starting
            || voice.live.active.is_some()
            || voice.live.capture.is_some()
    };
    if busy {
        if app
            .state::<crate::AppState>()
            .orb_conversation_owner
            .compare_exchange(
                ORB_SESSION_PENDING,
                ORB_SESSION_NONE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            clear_orb_ptt_flags(&app);
        }
        return Err("another voice capture is already active".to_string());
    }
    let mode = {
        let state = app.state::<crate::AppState>();
        let config = state.config.lock().await;
        configured_orb_voice_mode(&config.orb.voice_mode)
    };
    // Both transports own sizeable async state machines. Keep either behind an
    // exact heap-erased boundary so the lightweight wake handoff never inherits
    // their frame on a default Tokio or test-thread stack.
    let result = match mode {
        OrbVoiceMode::Dictation => Box::pin(begin_orb_dictation(app.clone())).await,
        OrbVoiceMode::HandsFree | OrbVoiceMode::Realtime => {
            Box::pin(engage_live_ptt(app.clone(), Some(mode))).await
        },
    };
    if let Err(error) = result {
        if app
            .state::<crate::AppState>()
            .orb_conversation_owner
            .compare_exchange(
                ORB_SESSION_PENDING,
                ORB_SESSION_NONE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            clear_orb_ptt_flags(&app);
        }
        return Err(error);
    }
    Ok(())
}

/// End whichever transport the orb owns without making the synchronous
/// lifecycle reducer wait on socket, recorder, or playback teardown.
pub fn request_end_orb_conversation(app: AppHandle) {
    clear_orb_ptt_flags(&app);
    let owner = app
        .state::<crate::AppState>()
        .orb_conversation_owner
        .swap(ORB_SESSION_NONE, Ordering::AcqRel);
    if matches!(owner, ORB_SESSION_NONE | ORB_SESSION_PENDING) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        end_live_ptt_session_for_sequence(&app, owner).await;
        cancel_orb_dictation_capture_for_sequence(&app, owner).await;
    });
}

fn orb_conversation_is_pending(app: &AppHandle) -> bool {
    app.state::<crate::AppState>()
        .orb_conversation_owner
        .load(Ordering::Acquire)
        == ORB_SESSION_PENDING
}

fn orb_conversation_has_owner(app: &AppHandle) -> bool {
    app.state::<crate::AppState>()
        .orb_conversation_owner
        .load(Ordering::Acquire)
        != ORB_SESSION_NONE
}

fn orb_conversation_owns(app: &AppHandle, sequence: u64) -> bool {
    app.state::<crate::AppState>()
        .orb_conversation_owner
        .load(Ordering::Acquire)
        == sequence
}

fn release_orb_conversation(app: &AppHandle, sequence: u64) -> bool {
    let released = release_orb_session(
        &app.state::<crate::AppState>().orb_conversation_owner,
        sequence,
    );
    if released {
        clear_orb_ptt_flags(app);
    }
    released
}

fn clear_orb_ptt_flags(app: &AppHandle) {
    let state = app.state::<crate::AppState>();
    state.orb_ptt_session.store(false, Ordering::Release);
    state.orb_ptt_down.store(false, Ordering::Release);
}

async fn begin_orb_dictation(app: AppHandle) -> Result<(), String> {
    let (sequence, capture) = start_orb_dictation_capture(&app, None).await?;
    crate::orb_window::dispatch(&app, crate::orb_state::OrbAction::Connected);
    let loop_app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = run_orb_dictation_loop(loop_app.clone(), sequence, capture).await {
            warn!(sequence, "Ambient Dictation failed: {error}");
            finish_orb_dictation_session(&loop_app, sequence, true);
        }
    });
    Ok(())
}

async fn start_orb_dictation_capture(
    app: &AppHandle,
    sequence: Option<u64>,
) -> Result<(u64, AmbientDictationCapture), String> {
    let sequence = {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        if voice.note.starting || voice.note.active.is_some() {
            return Err("another voice capture is already active".to_string());
        }
        if let Some(sequence) = sequence {
            if !orb_conversation_owns(app, sequence) {
                return Err("orb conversation was cancelled".to_string());
            }
            voice.note.starting = true;
            sequence
        } else {
            if !orb_conversation_is_pending(app) {
                return Err("orb conversation was cancelled".to_string());
            }
            voice.note.starting = true;
            voice.note.counter = next_orb_sequence(voice.note.counter);
            voice.note.counter
        }
    };

    let level_meter = Arc::new(AtomicU32::new(0));
    let recorder_meter = Arc::clone(&level_meter);
    let recording = match tokio::task::spawn_blocking(move || {
        spawn_recording_thread_with_meter(Some(recorder_meter))
    })
    .await
    {
        Ok(Ok(recording)) => recording,
        Ok(Err(error)) => {
            clear_orb_dictation_starting(app, sequence).await;
            return Err(format!(
                "Ambient Dictation could not access the microphone: {error}"
            ));
        },
        Err(error) => {
            clear_orb_dictation_starting(app, sequence).await;
            return Err(format!(
                "Ambient Dictation recording thread setup failed: {error}"
            ));
        },
    };

    let admitted = if orb_conversation_is_pending(app) {
        claim_orb_session(
            &app.state::<crate::AppState>().orb_conversation_owner,
            sequence,
        )
    } else {
        orb_conversation_owns(app, sequence)
    };
    if !admitted {
        stop_detached_recording(recording).await;
        clear_orb_dictation_starting(app, sequence).await;
        return Err("orb conversation was cancelled".to_string());
    }

    let mut recording = Some(recording);
    let stored = {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        if !orb_conversation_owns(app, sequence)
            || voice.note.active.is_some()
            || voice.note.counter != sequence
        {
            if voice.note.counter == sequence {
                voice.note.starting = false;
                voice.note.stop_after_start = false;
            }
            false
        } else if let Some(recording) = recording.take() {
            voice.note.starting = false;
            voice.note.stop_after_start = false;
            voice.note.active = Some(ActiveVoiceNote {
                stop_tx: recording.stop_tx,
                join_handle: recording.join_handle,
                started_at: Instant::now(),
                sequence,
                external_voice_token: None,
            });
            true
        } else {
            voice.note.starting = false;
            false
        }
    };
    if !stored {
        if let Some(recording) = recording {
            stop_detached_recording(recording).await;
        }
        release_orb_conversation(app, sequence);
        return Err("orb conversation was cancelled".to_string());
    }

    emit_voice_note_state(
        app,
        "ambient_dictation_listening",
        "Ambient Dictation is listening...",
        Some(sequence),
    );
    refresh_voice_menu(app).await;
    Ok((sequence, AmbientDictationCapture { level_meter }))
}

async fn clear_orb_dictation_starting(app: &AppHandle, sequence: u64) {
    let state = app.state::<crate::AppState>();
    let mut voice = state.voice_hotkeys.lock().await;
    if voice.note.counter == sequence {
        voice.note.starting = false;
        voice.note.stop_after_start = false;
    }
}

async fn stop_detached_recording(recording: RecordingThread) {
    let _ = recording.stop_tx.send(());
    let _ = tokio::task::spawn_blocking(move || recording.join_handle.join()).await;
}

async fn wait_for_orb_dictation_boundary(
    app: &AppHandle,
    sequence: u64,
    level_meter: &AtomicU32,
) -> AmbientDictationBoundary {
    let no_speech_after = {
        let seconds = app
            .state::<crate::AppState>()
            .config
            .lock()
            .await
            .orb
            .follow_up_seconds
            .max(1);
        Duration::from_secs(seconds)
    };
    let started = Instant::now();
    let mut gate = AmbientDictationSilenceGate::new(no_speech_after);
    let mut interval = tokio::time::interval(AMBIENT_DICTATION_POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if !orb_conversation_owns(app, sequence) {
            return AmbientDictationBoundary::Cancelled;
        }
        let press_to_talk = app
            .state::<crate::AppState>()
            .orb_ptt_session
            .load(Ordering::Acquire);
        let holding = app
            .state::<crate::AppState>()
            .orb_ptt_down
            .load(Ordering::Acquire);
        if press_to_talk && !holding && started.elapsed() > Duration::from_millis(120) {
            return gate.hold_release_boundary();
        }
        let level = f32::from_bits(level_meter.swap(0, Ordering::Relaxed));
        crate::orb_window::set_audio_level(app, "input", level);
        if let Some(boundary) = gate.observe(level, started.elapsed()) {
            return boundary;
        }
    }
}

async fn take_orb_dictation_recording(app: &AppHandle, sequence: u64) -> Option<ActiveVoiceNote> {
    let state = app.state::<crate::AppState>();
    let mut voice = state.voice_hotkeys.lock().await;
    let matches =
        voice.note.active.as_ref().is_some_and(|active| {
            active.sequence == sequence && active.external_voice_token.is_none()
        });
    if matches {
        voice.note.active.take()
    } else {
        None
    }
}

async fn finalize_orb_dictation_recording(
    app: &AppHandle,
    sequence: u64,
) -> Result<Option<CompletedVoiceNote>, String> {
    let Some(active) = take_orb_dictation_recording(app, sequence).await else {
        return Ok(None);
    };
    refresh_voice_menu(app).await;
    let _ = active.stop_tx.send(());
    tokio::task::spawn_blocking(move || {
        active
            .join_handle
            .join()
            .map_err(|_| "Ambient Dictation recording thread panicked".to_string())?
            .map(Some)
    })
    .await
    .map_err(|error| format!("joining Ambient Dictation recorder: {error}"))?
}

async fn cancel_orb_dictation_capture_for_sequence(app: &AppHandle, sequence: u64) {
    clear_orb_dictation_starting(app, sequence).await;
    if let Some(active) = take_orb_dictation_recording(app, sequence).await {
        let _ = active.stop_tx.send(());
        let _ = tokio::task::spawn_blocking(move || active.join_handle.join()).await;
    }
    refresh_voice_menu(app).await;
    crate::orb_window::set_audio_level(app, "input", 0.0);
    crate::orb_window::set_audio_level(app, "output", 0.0);
}

async fn run_orb_dictation_loop(
    app: AppHandle,
    sequence: u64,
    initial_capture: AmbientDictationCapture,
) -> Result<(), String> {
    let mut capture = initial_capture;
    let mut chat_session_id: Option<String> = None;
    let mut assistant_name: Option<String> = None;
    loop {
        // Every exit and hand-off below is logged at INFO: the loop used to be
        // silent on its normal paths, so a turn that ended without a reply
        // left nothing in the tray log to say whether the boundary, the
        // backend, or playback had ended it.
        let listening_since = Instant::now();
        let boundary = wait_for_orb_dictation_boundary(&app, sequence, &capture.level_meter).await;
        let completed = finalize_orb_dictation_recording(&app, sequence).await?;
        crate::orb_window::set_audio_level(&app, "input", 0.0);
        info!(
            sequence,
            boundary = ?boundary,
            listened_ms = listening_since.elapsed().as_millis() as u64,
            recorded_bytes = completed.as_ref().map(|note| note.bytes.len()).unwrap_or(0),
            "Ambient Dictation turn boundary"
        );
        if boundary == AmbientDictationBoundary::Cancelled || !orb_conversation_owns(&app, sequence)
        {
            info!(
                sequence,
                "Ambient Dictation session ended: cancelled or no longer owned"
            );
            return Ok(());
        }
        if boundary == AmbientDictationBoundary::HoldIdle {
            info!(sequence, "Ambient Dictation hold ended before speech");
            match continue_orb_dictation(&app, sequence).await? {
                Some(next) => {
                    capture = next;
                    continue;
                },
                None => return Ok(()),
            }
        }
        let Some(completed) = completed else {
            return Err("Ambient Dictation capture disappeared before completion".to_string());
        };
        if boundary == AmbientDictationBoundary::NoSpeech {
            info!(sequence, "Ambient Dictation session ended: no speech heard");
            finish_orb_dictation_session(&app, sequence, false);
            return Ok(());
        }

        crate::orb_window::dispatch(
            &app,
            crate::orb_state::OrbAction::Turn(crate::orb_state::OrbTurn::Thinking),
        );
        let submitted_at = Instant::now();
        let outcome = tokio::time::timeout(
            AMBIENT_DICTATION_MAX_TURN,
            submit_voice_note_for_surface(
                &app,
                completed.bytes,
                AMBIENT_DICTATION_SOURCE_SURFACE,
                chat_session_id.as_deref(),
            ),
        )
        .await
        .map_err(|_| "Ambient Dictation turn exceeded its bounded deadline".to_string())??;
        match &outcome {
            VoiceNoteSubmitOutcome::Accepted(turn) => info!(
                sequence,
                submit_ms = submitted_at.elapsed().as_millis() as u64,
                transcript_chars = turn.transcript.trim().chars().count(),
                preview_chars = turn
                    .assistant_preview
                    .as_deref()
                    .map(|text| text.trim().chars().count())
                    .unwrap_or(0),
                speech_segments = turn
                    .assistant_speech_segments
                    .as_ref()
                    .map(Vec::len)
                    .unwrap_or(0),
                queued = turn.queued_message_id.is_some(),
                "Ambient Dictation turn accepted by the backend"
            ),
            VoiceNoteSubmitOutcome::Rejected(rejection) => info!(
                sequence,
                submit_ms = submitted_at.elapsed().as_millis() as u64,
                code = rejection.code.as_deref().unwrap_or("unknown"),
                recoverable = rejection.is_recoverable_ambient_dictation(),
                "Ambient Dictation turn rejected by the backend"
            ),
        }
        let turn = match outcome {
            VoiceNoteSubmitOutcome::Accepted(turn) => turn,
            VoiceNoteSubmitOutcome::Rejected(rejection)
                if rejection.is_recoverable_ambient_dictation() =>
            {
                if let Some(rejected_session_id) = rejection.chat_session_id.clone() {
                    chat_session_id = Some(rejected_session_id);
                }
                let resolved_name = match assistant_name.as_ref() {
                    Some(name) => name.clone(),
                    None => {
                        let name = resolve_primary_agent_name(&app).await;
                        assistant_name = Some(name.clone());
                        name
                    },
                };
                let guidance = rejection.spoken_guidance();
                crate::orb_window::emit_caption(&app, "assistant", &resolved_name, &guidance, true);
                let feedback = VoiceNoteSubmitResponse {
                    chat_session_id: chat_session_id.clone().unwrap_or_default(),
                    chat_turn_id: None,
                    transcript: String::new(),
                    assistant_preview: Some(guidance),
                    assistant_speech_segments: None,
                    queued_message_id: None,
                };
                if let Err(error) = speak_orb_dictation_response(&app, sequence, &feedback).await {
                    warn!(
                        code = rejection.code.as_deref().unwrap_or("unknown"),
                        %error,
                        "Ambient Dictation could not speak recoverable backend guidance"
                    );
                }
                if !orb_conversation_owns(&app, sequence) {
                    return Ok(());
                }
                match continue_orb_dictation(&app, sequence).await? {
                    Some(next) => {
                        capture = next;
                        continue;
                    },
                    None => return Ok(()),
                }
            },
            VoiceNoteSubmitOutcome::Rejected(rejection) => return Err(rejection.to_string()),
        };
        if !orb_conversation_owns(&app, sequence) {
            return Ok(());
        }
        chat_session_id = Some(turn.chat_session_id.clone());
        let transcript = turn.transcript.trim();
        if !transcript.is_empty() {
            crate::orb_window::emit_caption(&app, "user", "You", transcript, true);
        }
        let resolved_name = match assistant_name.as_ref() {
            Some(name) => name.clone(),
            None => {
                let name = resolve_primary_agent_name(&app).await;
                assistant_name = Some(name.clone());
                name
            },
        };
        let assistant_text = turn
            .assistant_preview
            .as_deref()
            .map(strip_speech_tags_for_tts)
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .or_else(|| {
                turn.queued_message_id
                    .as_ref()
                    .map(|_| "I’m working on it.".to_string())
            });
        if let Some(text) = assistant_text.as_deref() {
            crate::orb_window::emit_caption(&app, "assistant", &resolved_name, text, true);
        }
        let spoke_at = Instant::now();
        speak_orb_dictation_response(&app, sequence, &turn).await?;
        info!(
            sequence,
            playback_ms = spoke_at.elapsed().as_millis() as u64,
            had_text = assistant_text.is_some(),
            "Ambient Dictation reply playback finished"
        );
        if !orb_conversation_owns(&app, sequence) {
            info!(
                sequence,
                "Ambient Dictation session ended: released during the reply"
            );
            return Ok(());
        }

        match continue_orb_dictation(&app, sequence).await? {
            Some(next) => capture = next,
            None => return Ok(()),
        }
    }
}

/// Open the next dictation capture. A press-to-talk session waits with the
/// microphone closed until Left Option is held again.
async fn continue_orb_dictation(
    app: &AppHandle,
    sequence: u64,
) -> Result<Option<AmbientDictationCapture>, String> {
    if app
        .state::<crate::AppState>()
        .orb_ptt_session
        .load(Ordering::Acquire)
    {
        crate::orb_window::dispatch(app, crate::orb_state::OrbAction::HoldReady);
        if !wait_for_orb_dictation_hold(app, sequence).await {
            return Ok(None);
        }
    }
    let (_, capture) = start_orb_dictation_capture(app, Some(sequence)).await?;
    crate::orb_window::dispatch(
        app,
        crate::orb_state::OrbAction::Turn(crate::orb_state::OrbTurn::Listening),
    );
    Ok(Some(capture))
}

async fn wait_for_orb_dictation_hold(app: &AppHandle, sequence: u64) -> bool {
    loop {
        if !orb_conversation_owns(app, sequence) {
            return false;
        }
        if app
            .state::<crate::AppState>()
            .orb_ptt_down
            .load(Ordering::Acquire)
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

fn finish_orb_dictation_session(app: &AppHandle, sequence: u64, failed: bool) {
    if !release_orb_conversation(app, sequence) {
        return;
    }
    crate::orb_window::set_audio_level(app, "input", 0.0);
    crate::orb_window::set_audio_level(app, "output", 0.0);
    crate::orb_window::dispatch(
        app,
        if failed {
            crate::orb_state::OrbAction::Disarm {
                reason: crate::orb_state::OrbEndedReason::SessionFailed,
            }
        } else {
            crate::orb_state::OrbAction::ConversationEnded
        },
    );
}

async fn resolve_primary_agent_name(app: &AppHandle) -> String {
    let url = app
        .state::<crate::AppState>()
        .config
        .lock()
        .await
        .engine_url("/api/magician/v2/agents?limit=100");
    let resolved = async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .ok()?;
        let response = crate::magician_auth::authorize(client.get(url))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let payload = response.json::<Value>().await.ok()?;
        primary_agent_name_from_payload(&payload)
    }
    .await;
    resolved.unwrap_or_else(|| ASSISTANT_FALLBACK_NAME.to_string())
}

fn primary_agent_name_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("agents")?
        .as_array()?
        .iter()
        .filter_map(|record| record.get("definition"))
        .find(|definition| {
            definition
                .get("is_primary")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .and_then(|definition| definition.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

async fn speak_orb_dictation_response(
    app: &AppHandle,
    sequence: u64,
    response: &VoiceNoteSubmitResponse,
) -> Result<(), String> {
    let config = app.state::<crate::AppState>().config.lock().await.clone();
    if config.voice.output_muted {
        info!(
            sequence,
            "Ambient Dictation reply not spoken: output is muted"
        );
        return Ok(());
    }
    let segments = voice_response_segments_for_tts(response);
    if segments.is_empty() {
        info!(
            sequence,
            "Ambient Dictation reply not spoken: nothing speakable in the response"
        );
        return Ok(());
    }
    let audio_segments = synthesize_mascot_tts_segments(&config, response, segments).await?;
    if audio_segments.is_empty() || !orb_conversation_owns(app, sequence) {
        info!(
            sequence,
            synthesized = audio_segments.len(),
            owned = orb_conversation_owns(app, sequence),
            "Ambient Dictation reply not spoken: no audio or the session was released"
        );
        return Ok(());
    }
    crate::orb_window::dispatch(
        app,
        crate::orb_state::OrbAction::Turn(crate::orb_state::OrbTurn::Speaking),
    );
    let output_meter = Arc::clone(&app.state::<crate::AppState>().orb_output_level);
    let playback_app = app.clone();
    tokio::task::spawn_blocking(move || {
        play_mascot_tts_segments(
            audio_segments,
            Some(output_meter),
            Some(Box::new(move || {
                orb_conversation_owns(&playback_app, sequence)
            })),
        )
    })
    .await
    .map_err(|error| format!("joining Ambient Dictation TTS playback: {error}"))??;
    crate::orb_window::set_audio_level(app, "output", 0.0);
    Ok(())
}

fn begin_external_voice(app: &AppHandle) -> Result<u64, String> {
    let state = app.state::<crate::AppState>();
    let mut token = state
        .external_voice_generation
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    if token == ORB_SESSION_NONE {
        token = state
            .external_voice_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
    }
    state
        .external_voice_owner
        .compare_exchange(ORB_SESSION_NONE, token, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "another external voice capture owns the microphone".to_string())?;
    crate::orb_window::dispatch(app, crate::orb_state::OrbAction::ExternalVoiceStarted);
    crate::voice_wake::suspend_native_wake(app);
    Ok(token)
}

fn end_external_voice(app: &AppHandle, token: u64) -> bool {
    let released =
        release_external_voice_owner(&app.state::<crate::AppState>().external_voice_owner, token);
    if released {
        crate::orb_window::dispatch(app, crate::orb_state::OrbAction::ExternalVoiceEnded);
        let orb_enabled = app
            .state::<crate::AppState>()
            .orb_enabled
            .load(Ordering::Acquire);
        if !orb_enabled {
            // ExternalVoiceEnded is intentionally a no-op when the orb is off,
            // so restore a legacy webview-owned detector explicitly. When the
            // orb is enabled, its lifecycle transition owns resume policy;
            // paused/disarming states must stay silent.
            crate::voice_wake::resume_native_wake(app);
        }
    }
    released
}

/// Mirror the web's universal Call/Dictate switch onto the native Left-Option
/// push-to-talk hold. Called from the webview whenever `voiceModeStore` changes
/// (and once on load). Accepts the web mode names (`realtime`/`recording`) and
/// their aliases.
#[tauri::command]
pub fn set_ptt_mode(app: AppHandle, mode: String) -> Result<(), String> {
    let value = match mode.trim().to_ascii_lowercase().as_str() {
        "realtime" | "live" | "call" => PTT_MODE_LIVE,
        "recording" | "dictate" | "dictation" => PTT_MODE_DICTATE,
        other => return Err(format!("unknown push-to-talk mode '{other}'")),
    };
    let previous = app
        .state::<crate::AppState>()
        .ptt_mode
        .swap(value, std::sync::atomic::Ordering::Relaxed);
    if previous != value {
        tauri::async_runtime::spawn(async move {
            refresh_voice_menu(&app).await;
            crate::commands::emit_hotkey_mappings_updated(&app);
        });
    }
    Ok(())
}

#[tauri::command]
pub async fn toggle_live_ptt_mute(app: AppHandle) -> Result<(), String> {
    let (muted, active) = {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        voice.live.muted = !voice.live.muted;
        let muted = voice.live.muted;
        let active = voice.live.active.as_ref().map(|active| {
            (
                active.control_tx.clone(),
                active.session_id.clone(),
                active.sequence,
            )
        });
        if muted {
            voice.live.capture = None;
        }
        (muted, active)
    };

    if let Some((tx, session_id, sequence)) = active.as_ref() {
        if muted {
            let _ = send_live_ptt_control(tx, LivePttControlCommand::ClearInput).await;
        }
        spawn_live_ptt_session_event(
            &app,
            session_id.clone(),
            MEDIA_VOICE_CONTROLLER_COMMAND,
            json!({
                "command": if muted { "mute" } else { "unmute" },
                "sequence": sequence,
            }),
        );
    }

    emit_voice_note_state(
        &app,
        if muted {
            "live_ptt_muted"
        } else {
            "live_ptt_unmuted"
        },
        if muted {
            "Live PTT microphone muted."
        } else {
            "Live PTT microphone unmuted."
        },
        active.as_ref().map(|(_, _, sequence)| *sequence),
    );
    let visual_state = if muted || active.is_none() {
        "idle"
    } else {
        "listening"
    };
    let _ = set_voice_visual(&app, visual_state).await;
    let _ = show_voice_bubble(
        &app,
        if muted {
            "Live PTT muted."
        } else {
            "Live PTT unmuted."
        },
    )
    .await;
    refresh_voice_menu(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn toggle_voice_output_mute(app: AppHandle) -> Result<(), String> {
    let muted = {
        let state = app.state::<crate::AppState>();
        let mut current = state.config.lock().await;
        let mut next = current.clone();
        next.voice.output_muted = !next.voice.output_muted;
        save_config_to_disk(&next)?;
        *current = next;
        current.voice.output_muted
    };
    let active_sequence = apply_live_ptt_output_mute(&app, muted).await;

    emit_voice_note_state(
        &app,
        if muted {
            "voice_output_muted"
        } else {
            "voice_output_unmuted"
        },
        if muted {
            "Assistant audio muted."
        } else {
            "Assistant audio unmuted."
        },
        active_sequence,
    );
    let _ = show_voice_bubble(
        &app,
        if muted {
            "Assistant audio muted. I will show text only."
        } else {
            "Assistant audio unmuted."
        },
    )
    .await;
    refresh_voice_menu(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn set_tutor_audio_focus(app: AppHandle, active: bool) -> Result<(), String> {
    let output_muted = if active {
        true
    } else {
        let state = app.state::<crate::AppState>();
        let config = state.config.lock().await;
        config.voice.output_muted
    };
    apply_live_ptt_output_mute(&app, output_muted).await;
    Ok(())
}

async fn apply_live_ptt_output_mute(app: &AppHandle, muted: bool) -> Option<u64> {
    let active = {
        let state = app.state::<crate::AppState>();
        let voice = state.voice_hotkeys.lock().await;
        voice
            .live
            .active
            .as_ref()
            .map(|active| (active.control_tx.clone(), active.sequence))
    };
    if let Some((tx, sequence)) = active.as_ref() {
        let _ = send_live_ptt_control(tx, LivePttControlCommand::SetOutputMuted(muted)).await;
        Some(*sequence)
    } else {
        None
    }
}

#[tauri::command]
pub async fn interrupt_live_ptt(app: AppHandle) -> Result<(), String> {
    let active = {
        let state = app.state::<crate::AppState>();
        let voice = state.voice_hotkeys.lock().await;
        voice.live.active.as_ref().map(|active| {
            (
                active.control_tx.clone(),
                active.session_id.clone(),
                active.sequence,
            )
        })
    };
    let Some((tx, session_id, sequence)) = active else {
        return Ok(());
    };
    let _ = send_live_ptt_control(&tx, LivePttControlCommand::Interrupt).await;
    spawn_live_ptt_session_event(
        &app,
        session_id,
        MEDIA_VOICE_CONTROLLER_COMMAND,
        json!({
            "command": "interrupt",
            "sequence": sequence,
        }),
    );
    emit_voice_note_state(
        &app,
        "live_ptt_interrupted",
        "Live PTT assistant speech interrupted.",
        Some(sequence),
    );
    let _ = set_voice_visual(&app, "idle").await;
    let _ = show_voice_bubble(&app, "Interrupted.").await;
    Ok(())
}

pub async fn sync_voice_shortcuts(
    app: &AppHandle,
    config: &MagicianDesktopConfig,
) -> Result<(), String> {
    let (previous_voice_note, previous_live_ptt) = {
        let state = app.state::<crate::AppState>();
        let voice = state.voice_hotkeys.lock().await;
        (
            voice.registered_voice_note_shortcut.clone(),
            voice.registered_live_ptt_shortcut.clone(),
        )
    };

    sync_single_shortcut(
        app,
        previous_voice_note.as_deref(),
        None,
        handle_voice_note_shortcut,
        "voice note",
    )?;
    sync_single_shortcut(
        app,
        previous_live_ptt.as_deref(),
        None,
        handle_live_ptt_shortcut,
        "live PTT",
    )?;

    {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        // Ambient Orb is the sole host-wide voice entry point. Keep the
        // serialized shortcut fields readable for config compatibility, but
        // actively unregister any shortcut left by an older desktop build.
        voice.registered_voice_note_shortcut = None;
        voice.registered_live_ptt_shortcut = None;
    }
    crate::voice_gesture::sync_voice_gestures(app, config).await?;
    apply_live_ptt_output_mute(app, config.voice.output_muted).await;

    let update_version = app
        .state::<crate::AppState>()
        .pending_app_update
        .lock()
        .await
        .clone();
    crate::tray::refresh_menu(app, update_version.as_deref());
    Ok(())
}

fn sync_single_shortcut(
    app: &AppHandle,
    previous: Option<&str>,
    desired: Option<&str>,
    handler: fn(&AppHandle, &Shortcut, ShortcutEvent),
    label: &str,
) -> Result<(), String> {
    if previous == desired {
        return Ok(());
    }

    if let Some(previous) = previous {
        if app.global_shortcut().is_registered(previous) {
            app.global_shortcut()
                .unregister(previous)
                .map_err(|e| format!("Failed to unregister {label} shortcut {previous}: {e}"))?;
        }
    }

    if let Some(desired) = desired {
        app.global_shortcut()
            .on_shortcut(desired, handler)
            .map_err(|e| format!("Failed to register {label} shortcut {desired}: {e}"))?;
        info!("Registered {label} global shortcut {desired}");
    }

    Ok(())
}

fn handle_voice_note_shortcut(app: &AppHandle, _shortcut: &Shortcut, event: ShortcutEvent) {
    match event.state {
        ShortcutState::Pressed => trigger_voice_note_start(app),
        ShortcutState::Released => trigger_voice_note_stop(app),
    }
}

pub(crate) fn trigger_voice_note_start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = start_voice_note_capture(app).await {
            warn!("Failed to start global voice note: {}", error);
        }
    });
}

pub(crate) fn trigger_voice_note_stop(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = stop_voice_note_capture(app).await {
            warn!("Failed to stop global voice note: {}", error);
        }
    });
}

fn handle_live_ptt_shortcut(app: &AppHandle, _shortcut: &Shortcut, event: ShortcutEvent) {
    match event.state {
        ShortcutState::Pressed => trigger_live_ptt_engage(app),
        ShortcutState::Released => trigger_live_ptt_release(app),
    }
}

pub(crate) fn trigger_live_ptt_engage(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = engage_live_ptt(app, None).await {
            warn!("Failed to engage live PTT: {}", error);
        }
    });
}

pub(crate) fn trigger_live_ptt_release(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = release_live_ptt(app).await {
            warn!("Failed to release live PTT: {}", error);
        }
    });
}

/// Cross the Live -> Hands-free failover through a fresh scheduler task.
///
/// Keeping this a synchronous scheduling boundary is deliberate: awaiting
/// `engage_live_ptt` from its own control-loop future creates a recursive async
/// type that cannot satisfy Tauri's `Send` task contract. It would also retain
/// the failed session's poll chain while constructing the replacement. The
/// ownership sentinel admits at most one replacement, and Hands-free never
/// schedules another fallback.
fn schedule_orb_hands_free_fallback(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = engage_live_ptt(app.clone(), Some(OrbVoiceMode::HandsFree)).await {
            warn!(error = %error, "Ambient Orb Hands-free fallback failed to start");
            let app_state = app.state::<crate::AppState>();
            let owner = &app_state.orb_conversation_owner;
            let released_pending = owner
                .compare_exchange(
                    ORB_SESSION_PENDING,
                    ORB_SESSION_NONE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok();
            if released_pending || owner.load(Ordering::Acquire) == ORB_SESSION_NONE {
                crate::orb_window::dispatch(
                    &app,
                    crate::orb_state::OrbAction::Disarm {
                        reason: crate::orb_state::OrbEndedReason::SessionFailed,
                    },
                );
            }
        }
    });
}

async fn engage_live_ptt(
    app: AppHandle,
    admitted_orb_mode: Option<OrbVoiceMode>,
) -> Result<(), String> {
    let orb_requested = admitted_orb_mode.is_some();
    let config = app.state::<crate::AppState>().config.lock().await.clone();
    if orb_requested && !orb_conversation_is_pending(&app) {
        return Err("orb conversation was cancelled".to_string());
    }
    if !orb_requested && orb_conversation_has_owner(&app) {
        return Err("the ambient orb currently owns live voice".to_string());
    }
    let maybe_existing = {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        if !orb_requested && orb_conversation_has_owner(&app) {
            return Err("the ambient orb currently owns live voice".to_string());
        }
        if voice.note.starting || voice.note.active.is_some() {
            return Err("a voice note currently owns the microphone".to_string());
        }
        if orb_requested
            && (voice.live.starting || voice.live.active.is_some() || voice.live.capture.is_some())
        {
            return Err("another voice capture is already active".to_string());
        }
        if voice.live.muted {
            if orb_requested {
                return Err("the live voice microphone is muted".to_string());
            }
            let sequence = voice.live.active.as_ref().map(|active| active.sequence);
            drop(voice);
            emit_voice_note_state(
                &app,
                "live_ptt_muted",
                "Live PTT microphone is muted.",
                sequence,
            );
            let _ = set_voice_visual(&app, "idle").await;
            let _ = show_voice_bubble(&app, "Live PTT is muted.").await;
            return Ok(());
        }
        if let Some(active) = voice.live.active.as_ref() {
            if voice.live.capture.is_some() {
                return Ok(());
            }
            Some((
                active.audio_tx.clone(),
                active.control_tx.clone(),
                active.sequence,
            ))
        } else if voice.live.starting {
            return Ok(());
        } else {
            voice.live.starting = true;
            voice.live.counter += 1;
            None
        }
    };
    let external_voice_token = if !orb_requested && maybe_existing.is_none() {
        match begin_external_voice(&app) {
            Ok(token) => Some(token),
            Err(error) => {
                clear_live_ptt_starting(&app).await;
                return Err(error);
            },
        }
    } else {
        None
    };
    if orb_requested {
        crate::voice_wake::suspend_native_wake(&app);
    }

    let (audio_tx, control_tx, sequence) = if let Some(existing) = maybe_existing {
        existing
    } else {
        let sequence = {
            let state = app.state::<crate::AppState>();
            let voice = state.voice_hotkeys.lock().await;
            voice.live.counter
        };
        emit_voice_note_state(
            &app,
            "live_ptt_starting",
            "Starting live PTT...",
            Some(sequence),
        );
        let _ = set_voice_visual(&app, "listening").await;
        let session = match register_live_ptt_media_session(&config).await {
            Ok(session) => session,
            Err(error) => {
                clear_live_ptt_starting(&app).await;
                if let Some(token) = external_voice_token {
                    end_external_voice(&app, token);
                }
                refresh_voice_menu(&app).await;
                let message = format!("Live PTT could not register a media session: {error}");
                emit_voice_note_state(&app, "live_ptt_error", &message, Some(sequence));
                let _ = set_voice_visual(&app, "failed").await;
                let _ = show_voice_bubble(&app, &message).await;
                return Err(message);
            },
        };
        if orb_requested
            && !claim_orb_session(
                &app.state::<crate::AppState>().orb_conversation_owner,
                sequence,
            )
        {
            let _ =
                disconnect_live_ptt_media_session(&app, &session.session_id, "orb_start_cancelled")
                    .await;
            clear_live_ptt_starting(&app).await;
            return Err("orb conversation was cancelled".to_string());
        }
        let (audio_tx, audio_rx) = tokio_mpsc::channel(LIVE_PTT_AUDIO_QUEUE_CAPACITY);
        let (control_tx, control_rx) = tokio_mpsc::channel(LIVE_PTT_CONTROL_QUEUE_CAPACITY);
        let thread_id = config.voice.default_thread_id.clone();
        let realtime_profile = config.voice.live_ptt_realtime_profile.clone();
        // Orb startup uses the mode admitted by `begin_orb_conversation`.
        // Re-reading `config.orb.voice_mode` here allowed a Settings change
        // during media-session registration to switch engines mid-connect.
        let voice_mode = live_ptt_wire_voice_mode(&config.voice.voice_mode, admitted_orb_mode)?;
        let press_to_talk = orb_requested
            && app
                .state::<crate::AppState>()
                .orb_ptt_session
                .load(Ordering::Acquire);
        let retain_connection =
            press_to_talk && matches!(admitted_orb_mode, Some(OrbVoiceMode::Realtime));
        let turn_boundary =
            live_ptt_turn_boundary(admitted_orb_mode, press_to_talk).map(str::to_string);
        let session_id = session.session_id.clone();
        {
            let state = app.state::<crate::AppState>();
            let mut voice = state.voice_hotkeys.lock().await;
            voice.live.starting = false;
            voice.live.active = Some(ActiveLivePtt {
                audio_tx: audio_tx.clone(),
                control_tx: control_tx.clone(),
                session_id: session_id.clone(),
                sequence,
                external_voice_token,
                assistant_name: None,
                retain_connection,
            });
            voice.live.input_speech_active = false;
            voice.live.active_output_response_id = None;
        }
        if orb_requested && !orb_conversation_owns(&app, sequence) {
            end_live_ptt_session_for_sequence(&app, sequence).await;
            let _ =
                disconnect_live_ptt_media_session(&app, &session_id, "orb_start_cancelled").await;
            return Err("orb conversation was cancelled".to_string());
        }
        let ws_app = app.clone();
        tauri::async_runtime::spawn(async move {
            let loop_result = run_live_ptt_control_loop(
                ws_app.clone(),
                session_id.clone(),
                thread_id,
                realtime_profile,
                voice_mode,
                turn_boundary,
                sequence,
                audio_rx,
                control_rx,
            )
            .await;
            let should_fallback_to_hands_free = should_fallback_orb_realtime(
                loop_result.is_err(),
                admitted_orb_mode,
                orb_conversation_owns(&ws_app, sequence),
            );
            if let Err(error) = &loop_result {
                warn!("Live PTT control loop failed: {}", error);
                if should_fallback_to_hands_free {
                    emit_voice_note_state(
                        &ws_app,
                        "live_ptt_degraded",
                        "Live realtime is unavailable; switching to Hands-free.",
                        Some(sequence),
                    );
                    crate::orb_window::dispatch(
                        &ws_app,
                        crate::orb_state::OrbAction::RecoverableError {
                            message: "Live realtime is unavailable; switching to Hands-free."
                                .to_string(),
                        },
                    );
                } else {
                    emit_voice_note_state(&ws_app, "live_ptt_error", &error, Some(sequence));
                    let _ = set_voice_visual(&ws_app, "failed").await;
                    let _ = show_voice_bubble(&ws_app, &format!("Live PTT failed: {error}")).await;
                }
                let _ =
                    disconnect_live_ptt_media_session(&ws_app, &session_id, "control_loop_failed")
                        .await;
            } else {
                let _ = set_voice_visual(&ws_app, "idle").await;
            }
            let (cleared, external_voice_token) =
                clear_live_ptt_session(&ws_app, &session_id).await;
            if cleared && release_orb_conversation(&ws_app, sequence) {
                if should_fallback_to_hands_free
                    && ws_app
                        .state::<crate::AppState>()
                        .orb_conversation_owner
                        .compare_exchange(
                            ORB_SESSION_NONE,
                            ORB_SESSION_PENDING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                {
                    info!(
                        failed_sequence = sequence,
                        "Ambient Orb falling back from Live realtime to Hands-free"
                    );
                    schedule_orb_hands_free_fallback(ws_app.clone());
                    return;
                }
                if loop_result.is_err() {
                    crate::orb_window::dispatch(
                        &ws_app,
                        crate::orb_state::OrbAction::Disarm {
                            reason: crate::orb_state::OrbEndedReason::SessionFailed,
                        },
                    );
                } else {
                    crate::orb_window::dispatch(
                        &ws_app,
                        crate::orb_state::OrbAction::ConversationEnded,
                    );
                }
            } else if cleared {
                if let Some(token) = external_voice_token {
                    end_external_voice(&ws_app, token);
                }
            }
        });
        refresh_voice_menu(&app).await;
        (audio_tx, control_tx, sequence)
    };

    if orb_requested && !orb_conversation_owns(&app, sequence) {
        end_live_ptt_session_for_sequence(&app, sequence).await;
        return Err("orb conversation was cancelled".to_string());
    }

    if let Err(error) = send_live_ptt_control(&control_tx, LivePttControlCommand::Engage).await {
        end_live_ptt_session_for_sequence(&app, sequence).await;
        release_orb_conversation(&app, sequence);
        refresh_voice_menu(&app).await;
        emit_voice_note_state(&app, "live_ptt_error", &error, Some(sequence));
        let _ = set_voice_visual(&app, "failed").await;
        let _ = show_voice_bubble(&app, &error).await;
        return Err(error);
    }
    if orb_requested && !orb_conversation_owns(&app, sequence) {
        end_live_ptt_session_for_sequence(&app, sequence).await;
        return Err("orb conversation was cancelled".to_string());
    }

    let capture_result = {
        // Keep the shared reservation while cpal opens. A failed/ended control
        // loop cannot clear the external lease and resume wake in the small
        // window before this new input stream is installed.
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        let still_active = voice
            .live
            .active
            .as_ref()
            .is_some_and(|active| active.sequence == sequence);
        if !still_active
            || voice.live.capture.is_some()
            || (orb_requested && !orb_conversation_owns(&app, sequence))
        {
            Err((
                "live voice ended while the microphone was opening".to_string(),
                false,
            ))
        } else {
            match LivePttCapture::start(app.clone(), audio_tx.clone(), sequence) {
                Ok(capture) => {
                    let input_label = capture.input_label.clone();
                    voice.live.capture = Some(capture);
                    Ok(input_label)
                },
                Err(error) => Err((
                    format!("Live PTT could not access the microphone: {error}"),
                    true,
                )),
            }
        }
    };
    let input_label = match capture_result {
        Ok(input_label) => input_label,
        Err((message, microphone_failed)) => {
            end_live_ptt_session_for_sequence(&app, sequence).await;
            release_orb_conversation(&app, sequence);
            refresh_voice_menu(&app).await;
            if microphone_failed {
                emit_voice_note_state(&app, "live_ptt_error", &message, Some(sequence));
                let _ = set_voice_visual(&app, "failed").await;
                let _ = show_voice_bubble(&app, &message).await;
            }
            return Err(message);
        },
    };
    if orb_requested
        && app
            .state::<crate::AppState>()
            .orb_ptt_session
            .load(Ordering::Acquire)
        && !app
            .state::<crate::AppState>()
            .orb_ptt_down
            .load(Ordering::Acquire)
    {
        // The key came up while the socket or microphone was still opening.
        // Commit whatever was captured and, for Live, leave the socket parked.
        return release_live_ptt(app).await;
    }
    emit_voice_note_state(
        &app,
        "live_ptt_listening",
        "Live PTT listening...",
        Some(sequence),
    );
    let _ = set_voice_visual(&app, "listening").await;
    let _ = show_voice_bubble(&app, &format!("Listening on {input_label}...")).await;
    refresh_voice_menu(&app).await;
    Ok(())
}

async fn release_live_ptt(app: AppHandle) -> Result<(), String> {
    let (control_tx, sequence, samples_sent, idle_generation, retain_connection) = {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        let Some(active) = voice.live.active.as_ref() else {
            voice.live.starting = false;
            return Ok(());
        };
        let control_tx = active.control_tx.clone();
        let sequence = active.sequence;
        let retain_connection = active.retain_connection;
        let Some(capture) = voice.live.capture.take() else {
            return Ok(());
        };
        let samples_sent = capture.samples_sent.load(Ordering::Relaxed);
        drop(capture);
        voice.live.idle_generation += 1;
        (
            control_tx,
            sequence,
            samples_sent,
            voice.live.idle_generation,
            retain_connection,
        )
    };
    let _ = send_live_ptt_control(&control_tx, LivePttControlCommand::Release).await;
    emit_voice_note_state(
        &app,
        "live_ptt_released",
        "Live PTT turn committed.",
        Some(sequence),
    );
    let _ = set_voice_visual(&app, "thinking").await;
    let _ = show_voice_bubble(&app, &format!("Thinking... ({samples_sent} samples)")).await;
    refresh_voice_menu(&app).await;
    if orb_conversation_owns(&app, sequence) {
        // The microphone is closed. Don't keep the orb in Listening.
        crate::orb_window::dispatch(
            &app,
            crate::orb_state::OrbAction::Turn(crate::orb_state::OrbTurn::Thinking),
        );
    }
    if retain_connection {
        spawn_retained_orb_ready(app, sequence, idle_generation);
    } else {
        spawn_live_ptt_idle_timeout(app, sequence, idle_generation);
    }
    Ok(())
}

/// After a Live press-to-talk release, return the orb to "hold to talk"
/// without closing the socket. A newer hold or a real response invalidates
/// this generation the same way the disconnecting idle timer does.
fn spawn_retained_orb_ready(app: AppHandle, sequence: u64, idle_generation: u64) {
    tauri::async_runtime::spawn(async move {
        let quiet_for = {
            let state = app.state::<crate::AppState>();
            let config = state.config.lock().await;
            Duration::from_secs(config.orb.follow_up_seconds.max(1))
        };
        tokio::time::sleep(quiet_for).await;
        let still_parked =
            {
                let state = app.state::<crate::AppState>();
                let voice = state.voice_hotkeys.lock().await;
                voice.live.capture.is_none()
                    && voice.live.idle_generation == idle_generation
                    && voice.live.active.as_ref().is_some_and(|active| {
                        active.sequence == sequence && active.retain_connection
                    })
            };
        if still_parked && orb_conversation_owns(&app, sequence) {
            crate::orb_window::dispatch(&app, crate::orb_state::OrbAction::HoldReady);
            let _ = set_voice_visual(&app, "idle").await;
        }
    });
}

fn spawn_live_ptt_idle_timeout(app: AppHandle, sequence: u64, idle_generation: u64) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(LIVE_PTT_IDLE_TIMEOUT).await;
        let maybe_session = {
            let state = app.state::<crate::AppState>();
            let mut voice = state.voice_hotkeys.lock().await;
            let should_end = voice.live.capture.is_none()
                && voice.live.idle_generation == idle_generation
                && voice
                    .live
                    .active
                    .as_ref()
                    .map(|active| active.sequence == sequence)
                    .unwrap_or(false);
            if should_end {
                voice.live.capture = None;
                voice.live.input_speech_active = false;
                voice.live.active_output_response_id = None;
                voice
                    .live
                    .active
                    .take()
                    .map(|active| (active.control_tx, active.external_voice_token))
            } else {
                None
            }
        };
        if let Some((tx, external_voice_token)) = maybe_session {
            let _ = send_live_ptt_control(&tx, LivePttControlCommand::End).await;
            if release_orb_conversation(&app, sequence) {
                crate::orb_window::dispatch(&app, crate::orb_state::OrbAction::ConversationEnded);
            } else if let Some(token) = external_voice_token {
                end_external_voice(&app, token);
            }
            emit_voice_note_state(
                &app,
                "live_ptt_idle_timeout",
                "Live PTT ended after being idle.",
                Some(sequence),
            );
            let _ = set_voice_visual(&app, "idle").await;
            let _ = show_voice_bubble(&app, "Live PTT ended after being idle.").await;
            refresh_voice_menu(&app).await;
        }
    });
}

async fn set_orb_streaming_speech_active(app: &AppHandle, sequence: u64, active: bool) {
    let state = app.state::<crate::AppState>();
    let mut voice = state.voice_hotkeys.lock().await;
    if voice
        .live
        .active
        .as_ref()
        .is_some_and(|session| session.sequence == sequence)
    {
        voice.live.input_speech_active = active;
        // Every semantic phase edge invalidates any older quiet timer. A new
        // timer is armed only at a boundary that is genuinely safe to close.
        voice.live.idle_generation = voice.live.idle_generation.saturating_add(1);
    }
}

async fn invalidate_orb_streaming_idle_timeout(app: &AppHandle, sequence: u64) {
    let state = app.state::<crate::AppState>();
    let mut voice = state.voice_hotkeys.lock().await;
    if voice
        .live
        .active
        .as_ref()
        .is_some_and(|session| session.sequence == sequence)
    {
        voice.live.idle_generation = voice.live.idle_generation.saturating_add(1);
    }
}

fn orb_streaming_is_semantically_quiet(
    active_sequence: Option<u64>,
    sequence: u64,
    input_speech_active: bool,
    active_output_response_id: Option<&str>,
) -> bool {
    active_sequence == Some(sequence) && !input_speech_active && active_output_response_id.is_none()
}

/// After a streaming turn goes quiet, either park a retained Live socket or
/// return to open-microphone listening and arm the disconnect deadline.
async fn settle_orb_after_streaming_quiet(app: &AppHandle, sequence: u64) {
    if !orb_conversation_owns(app, sequence) {
        return;
    }
    let (retained, mic_open) = {
        let state = app.state::<crate::AppState>();
        let voice = state.voice_hotkeys.lock().await;
        let retained = voice
            .live
            .active
            .as_ref()
            .is_some_and(|active| active.sequence == sequence && active.retain_connection);
        (retained, voice.live.capture.is_some())
    };
    if retained && !mic_open {
        crate::orb_window::dispatch(app, crate::orb_state::OrbAction::HoldReady);
        let _ = set_voice_visual(app, "idle").await;
        return;
    }
    crate::orb_window::dispatch(
        app,
        crate::orb_state::OrbAction::Turn(crate::orb_state::OrbTurn::Listening),
    );
    let _ = set_voice_visual(app, "listening").await;
    arm_orb_streaming_idle_timeout(app, sequence).await;
}

/// Arm a semantic quiet boundary for an Orb-owned streaming session.
///
/// Microphone PCM itself is never activity: an open device produces frames
/// forever, including room noise and speaker echo. Only server/provider
/// lifecycle events arm or invalidate this generation-scoped deadline.
/// A retained Live press-to-talk socket skips this; releasing the key must
/// not tear the connection down.
async fn arm_orb_streaming_idle_timeout(app: &AppHandle, sequence: u64) {
    if !orb_conversation_owns(app, sequence) {
        return;
    }
    let retained = {
        let state = app.state::<crate::AppState>();
        let voice = state.voice_hotkeys.lock().await;
        voice
            .live
            .active
            .as_ref()
            .is_some_and(|active| active.sequence == sequence && active.retain_connection)
    };
    if retained {
        return;
    }
    let quiet_for = {
        let state = app.state::<crate::AppState>();
        let config = state.config.lock().await;
        Duration::from_secs(config.orb.follow_up_seconds.max(1))
    };
    let idle_generation = {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        let safe_to_arm = orb_streaming_is_semantically_quiet(
            voice.live.active.as_ref().map(|session| session.sequence),
            sequence,
            voice.live.input_speech_active,
            voice.live.active_output_response_id.as_deref(),
        );
        if !safe_to_arm {
            return;
        }
        voice.live.idle_generation = voice.live.idle_generation.saturating_add(1);
        voice.live.idle_generation
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(quiet_for).await;
        let maybe_session = {
            let state = app.state::<crate::AppState>();
            let mut voice = state.voice_hotkeys.lock().await;
            let should_end = voice.live.idle_generation == idle_generation
                && orb_streaming_is_semantically_quiet(
                    voice.live.active.as_ref().map(|active| active.sequence),
                    sequence,
                    voice.live.input_speech_active,
                    voice.live.active_output_response_id.as_deref(),
                );
            if should_end {
                voice.live.capture = None;
                voice.live.input_speech_active = false;
                voice.live.active_output_response_id = None;
                voice
                    .live
                    .active
                    .take()
                    .map(|active| (active.control_tx, active.external_voice_token))
            } else {
                None
            }
        };
        if let Some((tx, external_voice_token)) = maybe_session {
            let _ = send_live_ptt_control(&tx, LivePttControlCommand::End).await;
            if release_orb_conversation(&app, sequence) {
                crate::orb_window::dispatch(&app, crate::orb_state::OrbAction::ConversationEnded);
            } else if let Some(token) = external_voice_token {
                end_external_voice(&app, token);
            }
            emit_voice_note_state(
                &app,
                "orb_streaming_idle_timeout",
                "Conversation quiet; listening for the wake phrase.",
                Some(sequence),
            );
            let _ = set_voice_visual(&app, "idle").await;
            refresh_voice_menu(&app).await;
        }
    });
}

async fn end_live_ptt_session(app: AppHandle, reason: &'static str) -> Result<(), String> {
    let ended = {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        voice.live.starting = false;
        voice.live.capture = None;
        voice.live.input_speech_active = false;
        voice.live.active_output_response_id = None;
        voice.live.active.take().map(|active| {
            (
                active.control_tx,
                active.session_id,
                active.sequence,
                active.external_voice_token,
            )
        })
    };
    let Some((tx, session_id, sequence, external_voice_token)) = ended else {
        refresh_voice_menu(&app).await;
        return Ok(());
    };
    spawn_live_ptt_session_event(
        &app,
        session_id,
        MEDIA_VOICE_CONTROLLER_COMMAND,
        json!({
            "command": "session.end",
            "reason": reason,
            "sequence": sequence,
        }),
    );
    let _ = send_live_ptt_control(&tx, LivePttControlCommand::End).await;
    if release_orb_conversation(&app, sequence) {
        crate::orb_window::dispatch(&app, crate::orb_state::OrbAction::ConversationEnded);
    } else if let Some(token) = external_voice_token {
        end_external_voice(&app, token);
    }
    emit_voice_note_state(&app, "live_ptt_ended", "Live PTT ended.", Some(sequence));
    let _ = set_voice_visual(&app, "idle").await;
    let _ = show_voice_bubble(&app, "Live PTT ended.").await;
    refresh_voice_menu(&app).await;
    Ok(())
}

async fn end_live_ptt_session_for_sequence(app: &AppHandle, sequence: u64) {
    let maybe_session = {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        let should_end = voice
            .live
            .active
            .as_ref()
            .map(|active| active.sequence == sequence)
            .unwrap_or(false);
        if should_end {
            voice.live.starting = false;
            voice.live.capture = None;
            voice.live.input_speech_active = false;
            voice.live.active_output_response_id = None;
            voice
                .live
                .active
                .take()
                .map(|active| (active.control_tx, active.external_voice_token))
        } else {
            None
        }
    };
    if let Some((tx, external_voice_token)) = maybe_session {
        let _ = send_live_ptt_control(&tx, LivePttControlCommand::End).await;
        if let Some(token) = external_voice_token {
            end_external_voice(app, token);
        }
    }
}

async fn clear_live_ptt_starting(app: &AppHandle) {
    let state = app.state::<crate::AppState>();
    let mut voice = state.voice_hotkeys.lock().await;
    voice.live.starting = false;
}

/// Atomically release a matching session slot before its terminal lifecycle
/// transition. The returned external lease is sequence-scoped, so a stale
/// control loop cannot resume wake behind a newer voice capture.
async fn clear_live_ptt_session(app: &AppHandle, session_id: &str) -> (bool, Option<u64>) {
    let state = app.state::<crate::AppState>();
    let mut voice = state.voice_hotkeys.lock().await;
    let matches_active = voice
        .live
        .active
        .as_ref()
        .map(|active| active.session_id == session_id)
        .unwrap_or(false);
    let external_voice_token = if matches_active {
        let token = voice
            .live
            .active
            .take()
            .and_then(|active| active.external_voice_token);
        voice.live.capture = None;
        voice.live.starting = false;
        voice.live.input_speech_active = false;
        voice.live.active_output_response_id = None;
        token
    } else {
        None
    };
    drop(voice);
    refresh_voice_menu(app).await;
    (matches_active, external_voice_token)
}

async fn is_recording_or_starting(app: &AppHandle) -> bool {
    let state = app.state::<crate::AppState>();
    let voice = state.voice_hotkeys.lock().await;
    voice.note.starting || voice.note.active.is_some()
}

async fn start_voice_note_capture(app: AppHandle) -> Result<(), String> {
    let sequence = {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        if voice.note.active.is_some() {
            return Ok(());
        }
        if voice.note.starting {
            return Ok(());
        }
        if orb_conversation_has_owner(&app)
            || voice.live.starting
            || voice.live.active.is_some()
            || voice.live.capture.is_some()
        {
            return Err("another voice capture currently owns the microphone".to_string());
        }
        voice.note.starting = true;
        voice.note.stop_after_start = false;
        voice.note.counter += 1;
        voice.note.counter
    };

    let external_voice_token = match begin_external_voice(&app) {
        Ok(token) => token,
        Err(error) => {
            let state = app.state::<crate::AppState>();
            let mut voice = state.voice_hotkeys.lock().await;
            voice.note.starting = false;
            voice.note.stop_after_start = false;
            return Err(error);
        },
    };
    emit_voice_note_state(&app, "starting", "Starting voice note...", Some(sequence));
    refresh_voice_menu(&app).await;

    let recorder_result = match tokio::task::spawn_blocking(spawn_recording_thread).await {
        Ok(result) => result,
        Err(error) => {
            let state = app.state::<crate::AppState>();
            let mut voice = state.voice_hotkeys.lock().await;
            voice.note.starting = false;
            voice.note.stop_after_start = false;
            drop(voice);
            refresh_voice_menu(&app).await;

            let message = format!("Voice note recording thread setup failed: {error}");
            emit_voice_note_state(&app, "error", &message, Some(sequence));
            let _ = set_voice_visual(&app, "failed").await;
            spawn_voice_note_lifecycle_event(
                &app,
                MEDIA_VOICE_NOTE_RECORDING_FAILED,
                json!({
                    "sequence": sequence,
                    "error": "recording_thread_setup_failed",
                    "details": message.as_str(),
                }),
            );
            let _ = show_voice_bubble(&app, &message).await;
            end_external_voice(&app, external_voice_token);
            return Err(message);
        },
    };
    let stop_immediately: bool;
    match recorder_result {
        Ok(recording_thread) => {
            let state = app.state::<crate::AppState>();
            let mut voice = state.voice_hotkeys.lock().await;
            stop_immediately = voice.note.stop_after_start;
            voice.note.starting = false;
            voice.note.stop_after_start = false;
            voice.note.active = Some(ActiveVoiceNote {
                stop_tx: recording_thread.stop_tx,
                join_handle: recording_thread.join_handle,
                started_at: Instant::now(),
                sequence,
                external_voice_token: Some(external_voice_token),
            });
        },
        Err(error) => {
            let state = app.state::<crate::AppState>();
            let mut voice = state.voice_hotkeys.lock().await;
            voice.note.starting = false;
            voice.note.stop_after_start = false;
            drop(voice);
            refresh_voice_menu(&app).await;

            let message = format!("Voice note could not access the microphone: {error}");
            emit_voice_note_state(&app, "error", &message, Some(sequence));
            let _ = set_voice_visual(&app, "failed").await;
            spawn_voice_note_lifecycle_event(
                &app,
                MEDIA_VOICE_NOTE_RECORDING_FAILED,
                json!({
                    "sequence": sequence,
                    "error": "microphone_unavailable",
                    "details": message.as_str(),
                }),
            );
            let _ = show_voice_bubble(&app, &message).await;
            end_external_voice(&app, external_voice_token);
            return Err(message);
        },
    }

    emit_voice_note_state(&app, "recording", "Recording voice note...", Some(sequence));
    refresh_voice_menu(&app).await;
    spawn_voice_note_lifecycle_event(
        &app,
        MEDIA_VOICE_NOTE_RECORDING_STARTED,
        json!({ "sequence": sequence }),
    );
    spawn_voice_note_auto_stop(app.clone(), sequence);
    if stop_immediately {
        stop_voice_note_capture(app).await?;
    }
    Ok(())
}

async fn stop_voice_note_capture(app: AppHandle) -> Result<(), String> {
    let pending_start = {
        let state = app.state::<crate::AppState>();
        let mut voice = state.voice_hotkeys.lock().await;
        if voice.note.active.as_ref().is_some_and(|active| {
            active.external_voice_token.is_none() && orb_conversation_owns(&app, active.sequence)
        }) {
            return Err("the Ambient Orb owns this Dictation capture".to_string());
        }
        if voice.note.starting {
            voice.note.stop_after_start = true;
            return Ok(());
        }
        voice.note.active.take()
    };

    let Some(active) = pending_start else {
        return Ok(());
    };
    refresh_voice_menu(&app).await;
    let sequence = active.sequence;
    let external_voice_token = active.external_voice_token;
    let duration_ms = active.started_at.elapsed().as_millis();
    emit_voice_note_state(&app, "stopping", "Finishing voice note...", Some(sequence));

    let _ = active.stop_tx.send(());
    let completed = match tokio::task::spawn_blocking(move || {
        active
            .join_handle
            .join()
            .map_err(|_| "voice note recording thread panicked".to_string())?
    })
    .await
    {
        Ok(Ok(completed)) => completed,
        Ok(Err(error)) => {
            let message = format!("Voice note recording could not be finalized: {error}");
            emit_voice_note_state(&app, "error", &message, Some(sequence));
            let _ = set_voice_visual(&app, "failed").await;
            spawn_voice_note_lifecycle_event(
                &app,
                MEDIA_VOICE_NOTE_RECORDING_FAILED,
                json!({
                    "sequence": sequence,
                    "duration_ms": duration_ms,
                    "error": "recording_finalize_failed",
                    "details": message.as_str(),
                }),
            );
            let _ = show_voice_bubble(&app, &message).await;
            if let Some(token) = external_voice_token {
                end_external_voice(&app, token);
            }
            return Err(message);
        },
        Err(error) => {
            let message = format!("Voice note recording join failed: {error}");
            emit_voice_note_state(&app, "error", &message, Some(sequence));
            let _ = set_voice_visual(&app, "failed").await;
            spawn_voice_note_lifecycle_event(
                &app,
                MEDIA_VOICE_NOTE_RECORDING_FAILED,
                json!({
                    "sequence": sequence,
                    "duration_ms": duration_ms,
                    "error": "recording_join_failed",
                    "details": message.as_str(),
                }),
            );
            let _ = show_voice_bubble(&app, &message).await;
            if let Some(token) = external_voice_token {
                end_external_voice(&app, token);
            }
            return Err(message);
        },
    };
    if let Some(token) = external_voice_token {
        end_external_voice(&app, token);
    }
    info!(
        sequence,
        duration_ms,
        bytes = completed.bytes.len(),
        samples = completed.samples_written,
        input = completed.input_label,
        "Captured global voice note"
    );
    spawn_voice_note_lifecycle_event(
        &app,
        MEDIA_VOICE_NOTE_RECORDING_STOPPED,
        json!({
            "sequence": sequence,
            "duration_ms": duration_ms,
            "audio_bytes": completed.bytes.len(),
            "samples_written": completed.samples_written,
            "input_label": completed.input_label.as_str(),
        }),
    );

    emit_voice_note_state(
        &app,
        "transcribing",
        "Transcribing voice note...",
        Some(sequence),
    );
    let _ = set_voice_visual(&app, "transcribing").await;
    let _ = show_voice_bubble(&app, "Transcribing...").await;
    match submit_voice_note(&app, completed.bytes).await {
        Ok(response) => {
            info!(
                sequence,
                chat_session_id = response.chat_session_id.as_str(),
                "Submitted global voice note to assistant chat"
            );
            let _ = set_voice_visual(&app, voice_visual_state_for_response(&response)).await;
            let message = voice_note_success_message(&response);
            emit_voice_note_state(&app, "submitted", &message, Some(sequence));
            let _ = show_voice_bubble(&app, &message).await;
            spawn_mascot_tts_for_response(&app, response);
            Ok(())
        },
        Err(error) => {
            let message = format!("Voice note failed: {error}");
            emit_voice_note_state(&app, "error", &message, Some(sequence));
            let _ = set_voice_visual(&app, "failed").await;
            let _ = show_voice_bubble(&app, &message).await;
            Err(message)
        },
    }
}

async fn submit_voice_note(
    app: &AppHandle,
    audio_bytes: Vec<u8>,
) -> Result<VoiceNoteSubmitResponse, String> {
    match submit_voice_note_for_surface(app, audio_bytes, VOICE_NOTE_SOURCE_SURFACE, None).await? {
        VoiceNoteSubmitOutcome::Accepted(response) => Ok(response),
        VoiceNoteSubmitOutcome::Rejected(rejection) => Err(rejection.to_string()),
    }
}

async fn submit_voice_note_for_surface(
    app: &AppHandle,
    audio_bytes: Vec<u8>,
    source_surface: &str,
    chat_session_id: Option<&str>,
) -> Result<VoiceNoteSubmitOutcome, String> {
    let config = app.state::<crate::AppState>().config.lock().await.clone();
    let url = config.engine_url("/api/magician/v2/media/voice-notes");
    let part = multipart::Part::bytes(audio_bytes)
        .file_name(VOICE_NOTE_FILENAME)
        .mime_str(VOICE_NOTE_MIME_TYPE)
        .map_err(|error| format!("failed to build audio multipart part: {error}"))?;
    let mut form = multipart::Form::new()
        .part("file", part)
        .text("thread_id", config.voice.default_thread_id.clone())
        .text("source_surface", source_surface.to_string())
        .text("presence_session_id", presence_session_id())
        .text(
            "retain_audio",
            config.voice.retain_voice_note_audio.to_string(),
        );
    form = form.text("mode", "ask");
    if let Some(chat_session_id) = chat_session_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        form = form.text("chat_session_id", chat_session_id.to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(VOICE_NOTE_HTTP_TIMEOUT)
        .build()
        .map_err(|error| format!("failed to create HTTP client: {error}"))?;
    let response = crate::magician_auth::authorize(client.post(url.clone()))
        .multipart(form)
        .send()
        .await
        .map_err(|error| format!("failed to submit voice note to {url}: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("failed to read voice note response: {error}"))?;
    if !status.is_success() {
        return Ok(VoiceNoteSubmitOutcome::Rejected(
            VoiceNoteBackendRejection::from_response(status, body),
        ));
    }
    serde_json::from_str(&body)
        .map(VoiceNoteSubmitOutcome::Accepted)
        .map_err(|error| format!("failed to parse voice note response: {error}; body={body}"))
}

fn voice_note_success_message(response: &VoiceNoteSubmitResponse) -> String {
    let transcript = response.transcript.trim();
    let transcript = if transcript.chars().count() > 160 {
        format!("{}...", transcript.chars().take(160).collect::<String>())
    } else {
        transcript.to_string()
    };
    if let Some(preview) = response.assistant_preview.as_deref() {
        let preview = preview.trim();
        let preview = if preview.chars().count() > 180 {
            format!("{}...", preview.chars().take(180).collect::<String>())
        } else {
            preview.to_string()
        };
        format!("You: {transcript}\nAssistant: {preview}")
    } else if response.queued_message_id.is_some() {
        format!("You: {transcript}\nAssistant is working on it.")
    } else {
        format!("You: {transcript}")
    }
}

fn voice_visual_state_for_response(response: &VoiceNoteSubmitResponse) -> &'static str {
    if response.queued_message_id.is_some() && response.assistant_preview.is_none() {
        "thinking"
    } else {
        "speaking"
    }
}

fn spawn_mascot_tts_for_response(app: &AppHandle, response: VoiceNoteSubmitResponse) {
    if response.assistant_preview.is_none()
        && response
            .assistant_speech_segments
            .as_ref()
            .map(|segments| segments.is_empty())
            .unwrap_or(true)
    {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = maybe_speak_mascot_response(&app, response).await {
            warn!("Mascot TTS skipped/failed: {error}");
        }
    });
}

async fn maybe_speak_mascot_response(
    app: &AppHandle,
    response: VoiceNoteSubmitResponse,
) -> Result<(), String> {
    let config = app.state::<crate::AppState>().config.lock().await.clone();
    if config.voice.output_muted {
        info!("Mascot TTS skipped because host-native assistant audio is muted");
        return Ok(());
    }
    {
        let state = app.state::<crate::AppState>();
        let voice = state.voice_hotkeys.lock().await;
        if voice.live.active.is_some() || voice.live.starting {
            info!("Mascot TTS skipped because live PTT is active");
            return Ok(());
        }
    }
    match active_web_tts_surface_exists(&config).await {
        Ok(true) => {
            info!("Mascot TTS skipped because a web/HUD media surface can own assistant speech");
            return Ok(());
        },
        Ok(false) => {},
        Err(error) => {
            warn!("Mascot TTS skipped because media-session check failed: {error}");
            return Ok(());
        },
    }

    let segments = voice_response_segments_for_tts(&response);
    if segments.is_empty() {
        return Ok(());
    }
    let audio_segments = synthesize_mascot_tts_segments(&config, &response, segments).await?;
    if audio_segments.is_empty() {
        return Ok(());
    }
    tokio::task::spawn_blocking(move || play_mascot_tts_segments(audio_segments, None, None))
        .await
        .map_err(|error| format!("joining mascot TTS playback: {error}"))?
}

fn voice_response_segments_for_tts(response: &VoiceNoteSubmitResponse) -> Vec<VoiceSpeechSegment> {
    if let Some(segments) = response.assistant_speech_segments.as_ref() {
        let parsed: Vec<VoiceSpeechSegment> = segments
            .iter()
            .filter_map(|segment| {
                let text = segment.text.trim();
                if text.is_empty() {
                    return None;
                }
                Some(VoiceSpeechSegment {
                    text: text.to_string(),
                    emotion: segment.emotion.clone(),
                    style: segment.style.clone(),
                    pace: segment.pace.clone(),
                    voice_mode: segment.voice_mode.clone(),
                    emphasis: segment.emphasis.clone(),
                })
            })
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    response
        .assistant_preview
        .as_deref()
        .map(strip_speech_tags_for_tts)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .map(|text| {
            vec![VoiceSpeechSegment {
                text,
                emotion: None,
                style: None,
                pace: None,
                voice_mode: None,
                emphasis: None,
            }]
        })
        .unwrap_or_default()
}

fn strip_speech_tags_for_tts(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    let mut out = String::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        let lower_tail = &lower[index..];
        if lower_tail.starts_with("<speech") {
            if let Some(end) = raw[index..].find('>') {
                index += end + 1;
                continue;
            }
        }
        if lower_tail.starts_with("</speech>") {
            index += "</speech>".len();
            continue;
        }
        let Some(ch) = raw[index..].chars().next() else {
            break;
        };
        out.push(ch);
        index += ch.len_utf8();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn active_web_tts_surface_exists(config: &MagicianDesktopConfig) -> Result<bool, String> {
    let url = config.engine_url("/api/magician/v2/media/sessions");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| format!("creating media-session client: {error}"))?;
    let response = crate::magician_auth::authorize(client.get(url.clone()))
        .send()
        .await
        .map_err(|error| format!("querying media sessions at {url}: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("reading media-session response: {error}"))?;
    if !status.is_success() {
        return Err(format!("media sessions returned {status}: {body}"));
    }
    let envelope: MediaSessionListEnvelope = serde_json::from_str(&body)
        .map_err(|error| format!("parsing media-session response: {error}; body={body}"))?;
    Ok(envelope.sessions.into_iter().any(|session| {
        let surface = session.surface_type.as_str();
        let is_web_or_hud = surface == "web_desktop" || surface == "web_mobile";
        let is_connected = session.status.is_empty() || session.status == "connected";
        is_web_or_hud
            && is_connected
            && (session.capabilities.browser_tts
                || session.capabilities.provider_tts
                || session.capabilities.realtime_voice)
    }))
}

async fn synthesize_mascot_tts_segments(
    config: &MagicianDesktopConfig,
    response: &VoiceNoteSubmitResponse,
    segments: Vec<VoiceSpeechSegment>,
) -> Result<Vec<MascotTtsAudioSegment>, String> {
    let url = config.engine_url("/api/magician/v2/media/tts/synthesize_message");
    let request = MascotTtsRequest {
        provider: None,
        message_id: response.chat_turn_id.clone(),
        segments,
        format: "wav",
    };
    let client = reqwest::Client::builder()
        .timeout(VOICE_NOTE_HTTP_TIMEOUT)
        .build()
        .map_err(|error| format!("creating mascot TTS client: {error}"))?;
    let http_response = crate::magician_auth::authorize(client.post(url.clone()))
        .json(&request)
        .send()
        .await
        .map_err(|error| format!("calling mascot TTS endpoint {url}: {error}"))?;
    let status = http_response.status();
    if status.as_u16() == 204 {
        return Ok(Vec::new());
    }
    let body = http_response
        .text()
        .await
        .map_err(|error| format!("reading mascot TTS response: {error}"))?;
    if !status.is_success() {
        return Err(format!("mascot TTS returned {status}: {body}"));
    }

    let mut out = Vec::new();
    for line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let envelope: MascotTtsEnvelope = match serde_json::from_str(line) {
            Ok(envelope) => envelope,
            Err(error) => {
                warn!("Ignoring malformed mascot TTS envelope: {error}; line={line}");
                continue;
            },
        };
        match envelope.kind.as_str() {
            "segment" => {
                let Some(audio_b64) = envelope.audio_b64 else {
                    continue;
                };
                let content_type = envelope.content_type.unwrap_or_default();
                if !content_type.to_ascii_lowercase().contains("wav") {
                    warn!(
                        provider = envelope.provider.as_deref().unwrap_or("unknown"),
                        model = envelope.model.as_deref().unwrap_or("unknown"),
                        content_type,
                        "Mascot TTS segment was not WAV; cannot play it with native PCM output"
                    );
                    continue;
                }
                let audio = BASE64_STANDARD
                    .decode(audio_b64.as_bytes())
                    .map_err(|error| format!("decoding mascot TTS audio: {error}"))?;
                out.push(MascotTtsAudioSegment {
                    audio,
                    content_type,
                    provider: envelope.provider,
                    model: envelope.model,
                });
            },
            "error" => {
                warn!(
                    code = envelope.code.as_deref().unwrap_or("tts_error"),
                    message = envelope.message.as_deref().unwrap_or("unknown"),
                    "Mascot TTS segment failed"
                );
            },
            "done" => {},
            other => {
                warn!("Ignoring unknown mascot TTS envelope type: {other}");
            },
        }
    }
    Ok(out)
}

fn play_mascot_tts_segments(
    segments: Vec<MascotTtsAudioSegment>,
    output_meter: Option<Arc<AtomicU32>>,
    continue_playback: Option<Box<dyn Fn() -> bool + Send>>,
) -> Result<(), String> {
    let playback = LivePttPlayback::start()?;
    let always_continue = || true;
    let keep_playing: &dyn Fn() -> bool = continue_playback.as_deref().unwrap_or(&always_continue);
    let mut playback_started = false;
    let mut total_duration = Duration::ZERO;
    for segment in segments {
        if !segment.content_type.to_ascii_lowercase().contains("wav") {
            continue;
        }
        let source = format!(
            "TTS segment (provider={}, model={})",
            segment.provider.as_deref().unwrap_or("unknown"),
            segment.model.as_deref().unwrap_or("unknown")
        );
        let (bytes, duration) = wav_bytes_to_live_ptt_pcm16_from_source(&segment.audio, &source)?;
        if bytes.is_empty() {
            continue;
        }
        if !playback_started {
            // All backend speech segments belong to one assistant response.
            // Keep one prebuffer/drain lifecycle across semantic/emotion
            // boundaries so a new segment cannot inject silence mid-sentence.
            playback.begin_segment();
            playback_started = true;
        }
        total_duration = total_duration.saturating_add(duration);
        const PLAYBACK_CHUNK_BYTES: usize = (LIVE_PTT_WIRE_SAMPLE_RATE_HZ as usize / 20) * 2;
        for chunk in bytes.chunks(PLAYBACK_CHUNK_BYTES.max(2)) {
            if !playback.wait_for_producer_room(keep_playing) {
                playback.clear();
                return Ok(());
            }
            playback.push_pcm16_bytes_paced(chunk);
            if let Some(meter) = output_meter.as_ref() {
                store_peak_level(meter, pcm16_le_bytes_rms(chunk));
            }
        }
    }
    if playback_started {
        playback.end_segment();
        if !playback.wait_until_drained(
            keep_playing,
            total_duration.saturating_add(Duration::from_secs(5)),
        ) {
            playback.clear();
            if !keep_playing() {
                return Ok(());
            }
        }
    }
    drop(playback);
    Ok(())
}

fn emit_voice_note_state(app: &AppHandle, state: &str, message: &str, sequence: Option<u64>) {
    let _ = app.emit(
        VOICE_NOTE_EVENT,
        VoiceNoteStateEvent {
            state,
            message,
            sequence,
        },
    );
}

fn spawn_voice_note_lifecycle_event(app: &AppHandle, event_type: &'static str, payload: Value) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = post_voice_note_lifecycle_event(&app, event_type, payload).await {
            warn!("Failed to publish voice-note lifecycle event {event_type}: {error}");
        }
    });
}

async fn post_voice_note_lifecycle_event(
    app: &AppHandle,
    event_type: &'static str,
    payload: Value,
) -> Result<(), String> {
    let config = app.state::<crate::AppState>().config.lock().await.clone();
    let url = config.engine_url("/api/magician/v2/media/voice-notes/events");
    let payload = merge_voice_note_payload(payload, &config.voice.default_thread_id);
    let body = json!({
        "event_type": event_type,
        "payload": payload,
    });
    let client = reqwest::Client::builder()
        .timeout(VOICE_NOTE_EVENT_HTTP_TIMEOUT)
        .build()
        .map_err(|error| format!("failed to create voice-note event HTTP client: {error}"))?;
    let response = crate::magician_auth::authorize(client.post(url.clone()))
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("failed to publish voice-note event to {url}: {error}"))?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    Err(format!("The backend returned {status}: {text}"))
}

fn merge_voice_note_payload(payload: Value, thread_id: &str) -> Value {
    let mut object = match payload {
        Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".to_string(), other);
            map
        },
    };
    object.insert(
        "source_surface".to_string(),
        Value::String(VOICE_NOTE_SOURCE_SURFACE.to_string()),
    );
    object.insert(
        "presence_session_id".to_string(),
        Value::String(presence_session_id()),
    );
    object.insert(
        "thread_id".to_string(),
        Value::String(thread_id.to_string()),
    );
    Value::Object(object)
}

async fn set_voice_visual(_app: &AppHandle, _state: &str) -> Result<(), String> {
    Ok(())
}

fn spawn_voice_note_auto_stop(app: AppHandle, sequence: u64) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(VOICE_NOTE_MAX_DURATION).await;
        let should_stop = {
            let state = app.state::<crate::AppState>();
            let voice = state.voice_hotkeys.lock().await;
            voice
                .note
                .active
                .as_ref()
                .map(|active| active.sequence == sequence)
                .unwrap_or(false)
        };
        if should_stop {
            warn!(
                sequence,
                "Voice note hit max duration; stopping automatically"
            );
            let _ = stop_voice_note_capture(app).await;
        }
    });
}

async fn show_voice_bubble(_app: &AppHandle, _text: &str) -> Result<(), String> {
    Ok(())
}

async fn refresh_voice_menu(app: &AppHandle) {
    let update_version = app
        .state::<crate::AppState>()
        .pending_app_update
        .lock()
        .await
        .clone();
    crate::tray::refresh_menu(app, update_version.as_deref());
}

fn presence_session_id() -> String {
    format!("desktop-tray-{}", std::process::id())
}

async fn register_live_ptt_media_session(
    config: &MagicianDesktopConfig,
) -> Result<MediaSessionWire, String> {
    let url = config.engine_url("/api/magician/v2/media/sessions");
    let body = json!({
        "thread_id": config.voice.default_thread_id,
        "source_surface": LIVE_PTT_SOURCE_SURFACE,
        "presence_session_id": presence_session_id(),
        "surface_type": "tray_macos",
        "transport": "websocket",
        "capabilities": {
            "mascot_overlay": true,
            "text_bubble": true,
            "provider_tts": true,
            "realtime_voice": true,
            "mic": true,
        },
        "permissions": {
            "mic": "granted",
            "transcription": "granted",
            "raw_media_persistence": "denied",
        },
        "display_label": "Magican Desktop Live PTT",
        "user_agent": format!("magician-desktop/{}", env!("CARGO_PKG_VERSION")),
    });
    let client = reqwest::Client::builder()
        .timeout(LIVE_PTT_HTTP_TIMEOUT)
        .build()
        .map_err(|error| format!("failed to create live PTT HTTP client: {error}"))?;
    let response = crate::magician_auth::authorize(client.post(url.clone()))
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("failed to register live PTT session at {url}: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("failed to read live PTT session response: {error}"))?;
    if !status.is_success() {
        return Err(format!("The backend returned {status}: {body}"));
    }
    serde_json::from_str::<MediaSessionEnvelope>(&body)
        .map(|envelope| envelope.session)
        .map_err(|error| format!("failed to parse live PTT session response: {error}; body={body}"))
}

fn spawn_live_ptt_session_event(
    app: &AppHandle,
    session_id: String,
    event_type: &'static str,
    payload: Value,
) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) =
            post_live_ptt_session_event(&app, &session_id, event_type, payload).await
        {
            warn!("Failed to publish live PTT event {event_type}: {error}");
        }
    });
}

async fn post_live_ptt_session_event(
    app: &AppHandle,
    session_id: &str,
    event_type: &'static str,
    payload: Value,
) -> Result<(), String> {
    let config = app.state::<crate::AppState>().config.lock().await.clone();
    let url = config.engine_url(&format!(
        "/api/magician/v2/media/sessions/{session_id}/events"
    ));
    let body = json!({
        "event_type": event_type,
        "payload": merge_live_ptt_payload(payload, &config.voice.default_thread_id),
    });
    let client = reqwest::Client::builder()
        .timeout(VOICE_NOTE_EVENT_HTTP_TIMEOUT)
        .build()
        .map_err(|error| format!("failed to create live PTT event HTTP client: {error}"))?;
    let response = crate::magician_auth::authorize(client.post(url.clone()))
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("failed to publish live PTT event to {url}: {error}"))?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    Err(format!("The backend returned {status}: {text}"))
}

async fn post_live_ptt_heartbeat(app: &AppHandle, session_id: &str) -> Result<(), String> {
    let config = app.state::<crate::AppState>().config.lock().await.clone();
    let url = config.engine_url(&format!(
        "/api/magician/v2/media/sessions/{session_id}/heartbeat"
    ));
    let body = json!({});
    let client = reqwest::Client::builder()
        .timeout(VOICE_NOTE_EVENT_HTTP_TIMEOUT)
        .build()
        .map_err(|error| format!("failed to create live PTT heartbeat HTTP client: {error}"))?;
    let response = crate::magician_auth::authorize(client.post(url.clone()))
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("failed to heartbeat live PTT session at {url}: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Err(format!("The backend returned {status}: {text}"))
    }
}

async fn disconnect_live_ptt_media_session(
    app: &AppHandle,
    session_id: &str,
    _reason: &str,
) -> Result<(), String> {
    let config = app.state::<crate::AppState>().config.lock().await.clone();
    let url = config.engine_url(&format!("/api/magician/v2/media/sessions/{session_id}"));
    let client = reqwest::Client::builder()
        .timeout(VOICE_NOTE_EVENT_HTTP_TIMEOUT)
        .build()
        .map_err(|error| format!("failed to create live PTT disconnect HTTP client: {error}"))?;
    let response = crate::magician_auth::authorize(client.delete(url.clone()))
        .send()
        .await
        .map_err(|error| format!("failed to disconnect live PTT session at {url}: {error}"))?;
    if response.status().is_success() || response.status().as_u16() == 404 {
        Ok(())
    } else {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Err(format!("The backend returned {status}: {text}"))
    }
}

fn merge_live_ptt_payload(payload: Value, thread_id: &str) -> Value {
    let mut object = match payload {
        Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".to_string(), other);
            map
        },
    };
    object.insert(
        "source_surface".to_string(),
        Value::String(LIVE_PTT_SOURCE_SURFACE.to_string()),
    );
    object.insert(
        "presence_session_id".to_string(),
        Value::String(presence_session_id()),
    );
    object.insert(
        "thread_id".to_string(),
        Value::String(thread_id.to_string()),
    );
    Value::Object(object)
}

fn live_ptt_session_start_payload(
    thread_id: &str,
    realtime_profile: &str,
    voice_mode: &str,
    turn_boundary: Option<&str>,
) -> Value {
    let mut payload = json!({
        "ui_thread_id": thread_id,
        "thread_id": thread_id,
        "realtime_profile": realtime_profile,
        "voice_mode": voice_mode,
        "echo_cancellation": false,
    });
    if let Some(turn_boundary) = turn_boundary {
        payload["turn_boundary"] = Value::String(turn_boundary.to_string());
    }
    payload
}

async fn run_live_ptt_control_loop(
    app: AppHandle,
    session_id: String,
    thread_id: String,
    realtime_profile: String,
    voice_mode: String,
    turn_boundary: Option<String>,
    sequence: u64,
    mut audio_rx: tokio_mpsc::Receiver<Vec<u8>>,
    mut control_rx: tokio_mpsc::Receiver<LivePttControlCommand>,
) -> Result<(), String> {
    let (ws_url, mut output_muted) = {
        let state = app.state::<crate::AppState>();
        let config = state.config.lock().await;
        (
            config.engine_ws_path(&format!(
                "/api/magician/v2/media/voice/{session_id}/control"
            )),
            config.voice.output_muted,
        )
    };
    let ws_request = crate::magician_auth::websocket_request(&ws_url)?;
    let (ws, _) = match connect_async(ws_request).await {
        Ok(result) => result,
        Err(error) => {
            let message = format!("failed to open live PTT control WebSocket: {error}");
            post_live_ptt_session_event(
                &app,
                &session_id,
                MEDIA_VOICE_BRIDGE_ERROR,
                json!({
                    "stage": "connect",
                    "error": message.as_str(),
                    "sequence": sequence,
                }),
            )
            .await
            .ok();
            return Err(message);
        },
    };
    let (mut ws_write, mut ws_read) = ws.split();
    let mut input_gate = HalfDuplexInputGate::new(voice_mode.eq_ignore_ascii_case("hands_free"));
    let playback = match LivePttPlayback::start() {
        Ok(playback) => Some(playback),
        Err(error) => {
            warn!("Live PTT playback is unavailable: {}", error);
            post_live_ptt_session_event(
                &app,
                &session_id,
                MEDIA_VOICE_BRIDGE_ERROR,
                json!({
                    "stage": "playback_start",
                    "error": error,
                    "sequence": sequence,
                }),
            )
            .await
            .ok();
            None
        },
    };
    let result = async {
        let session_start = live_ptt_session_start_payload(
            &thread_id,
            &realtime_profile,
            &voice_mode,
            turn_boundary.as_deref(),
        );
        send_ws_json(&mut ws_write, "session.start", session_start).await?;
        post_live_ptt_session_event(
            &app,
            &session_id,
            MEDIA_VOICE_CONTROLLER_COMMAND,
            json!({
                "command": "session.start",
                "sequence": sequence,
            }),
        )
        .await
        .ok();

        let mut heartbeat = tokio::time::interval(LIVE_PTT_HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;

        loop {
            tokio::select! {
                biased;
                Some(command) = control_rx.recv() => {
                    match command {
                        LivePttControlCommand::Engage => {
                            if let Some(playback) = playback.as_ref() {
                                playback.clear();
                            }
                            // `ptt.engage` is also the first command after a
                            // wake-created session, when no provider response
                            // can exist. The controller/provider owns
                            // state-aware barge-in; emitting an unconditional
                            // interrupt here makes OpenAI reject every first
                            // wake with "no active response found".
                            send_ws_json(&mut ws_write, "ptt.engage", json!({})).await?;
                            post_live_ptt_session_event(
                                &app,
                                &session_id,
                                MEDIA_VOICE_CONTROLLER_COMMAND,
                                json!({
                                    "command": "ptt.engage",
                                    "sequence": sequence,
                                }),
                            )
                            .await
                            .ok();
                        },
                        LivePttControlCommand::Release => {
                            send_ws_json(&mut ws_write, "ptt.release", json!({})).await?;
                            post_live_ptt_session_event(
                                &app,
                                &session_id,
                                MEDIA_VOICE_CONTROLLER_COMMAND,
                                json!({
                                    "command": "ptt.release",
                                    "turn": "committed",
                                    "sequence": sequence,
                                }),
                            )
                            .await
                            .ok();
                        },
                        LivePttControlCommand::ClearInput => {
                            send_ws_json(&mut ws_write, "input.clear", json!({})).await?;
                            if let Some(playback) = playback.as_ref() {
                                playback.clear();
                            }
                            post_live_ptt_session_event(
                                &app,
                                &session_id,
                                MEDIA_VOICE_CONTROLLER_COMMAND,
                                json!({
                                    "command": "input.clear",
                                    "sequence": sequence,
                                }),
                            )
                            .await
                            .ok();
                        },
                        LivePttControlCommand::Interrupt => {
                            send_ws_json(&mut ws_write, "response.interrupt", json!({})).await?;
                            if let Some(playback) = playback.as_ref() {
                                playback.clear();
                            }
                            post_live_ptt_session_event(
                                &app,
                                &session_id,
                                MEDIA_VOICE_CONTROLLER_COMMAND,
                                json!({
                                    "command": "response.interrupt",
                                    "sequence": sequence,
                                }),
                            )
                            .await
                            .ok();
                        },
                        LivePttControlCommand::SetOutputMuted(muted) => {
                            output_muted = muted;
                            if muted {
                                if let Some(playback) = playback.as_ref() {
                                    playback.clear();
                                }
                            }
                            post_live_ptt_session_event(
                                &app,
                                &session_id,
                                MEDIA_VOICE_CONTROLLER_COMMAND,
                                json!({
                                    "command": if muted { "output.mute" } else { "output.unmute" },
                                    "sequence": sequence,
                                }),
                            )
                            .await
                            .ok();
                        },
                        LivePttControlCommand::End => {
                            let _ = send_ws_json(&mut ws_write, "session.end", json!({})).await;
                            post_live_ptt_session_event(
                                &app,
                                &session_id,
                                MEDIA_VOICE_CONTROLLER_COMMAND,
                                json!({
                                    "command": "session.end",
                                    "sequence": sequence,
                                }),
                            )
                            .await
                            .ok();
                            return Ok(());
                        },
                    }
                },
                _ = heartbeat.tick() => {
                    if let Err(error) = post_live_ptt_heartbeat(&app, &session_id).await {
                        post_live_ptt_session_event(
                            &app,
                            &session_id,
                            MEDIA_VOICE_BRIDGE_ERROR,
                            json!({
                                "stage": "heartbeat",
                                "error": error.as_str(),
                                "sequence": sequence,
                            }),
                        )
                        .await
                        .ok();
                        return Err(format!("live PTT heartbeat failed: {error}"));
                    }
                },
                maybe_msg = ws_read.next() => {
                    let Some(message) = maybe_msg else {
                        return Err("live PTT control WebSocket ended without a terminal session event".to_string());
                    };
                    match message {
                        Ok(Message::Text(text)) => {
                            let outcome = handle_live_ptt_control_message(
                                &app,
                                &session_id,
                                &text,
                                sequence,
                                output_muted,
                            ).await?;
                            if outcome.begin_playback_segment {
                                input_gate.note_output_started(Instant::now());
                                if let Some(playback) = playback.as_ref() {
                                    playback.begin_segment();
                                }
                            }
                            if outcome.end_playback_segment {
                                if let Some(playback) = playback.as_ref() {
                                    playback.end_streaming_segment();
                                }
                            }
                            if outcome.clear_playback {
                                if let Some(playback) = playback.as_ref() {
                                    playback.clear();
                                }
                            }
                            if !outcome.keep_open {
                                return Ok(());
                            }
                        },
                        Ok(Message::Binary(bytes)) => {
                            if !output_muted && playback.is_some() && orb_conversation_owns(&app, sequence) {
                                crate::orb_window::set_audio_level(
                                    &app,
                                    "output",
                                    pcm16_le_bytes_rms(&bytes),
                                );
                            }
                            if output_muted {
                                // Keep provider transcripts flowing while dropping host playback.
                            } else if let Some(playback) = playback.as_ref() {
                                playback.push_pcm16_bytes(&bytes);
                            } else {
                                post_live_ptt_session_event(
                                    &app,
                                    &session_id,
                                    MEDIA_VOICE_CLIENT_AUDIO,
                                    json!({
                                        "direction": "output",
                                        "dropped": true,
                                        "bytes": bytes.len(),
                                        "reason": "playback_unavailable",
                                        "sequence": sequence,
                                    }),
                                )
                                .await
                                .ok();
                            }
                        },
                        Ok(Message::Close(frame)) => {
                            return Err(format!(
                                "live PTT control WebSocket closed without a terminal session event: {frame:?}"
                            ));
                        },
                        Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {},
                        Err(error) => {
                            let message = format!("live PTT control WebSocket error: {error}");
                            post_live_ptt_session_event(
                                &app,
                                &session_id,
                                MEDIA_VOICE_BRIDGE_ERROR,
                                json!({
                                    "stage": "websocket",
                                    "error": message.as_str(),
                                    "sequence": sequence,
                                }),
                            )
                            .await
                            .ok();
                            return Err(message);
                        },
                    }
                },
                Some(frame) = audio_rx.recv() => {
                    let playback_active = playback
                        .as_ref()
                        .is_some_and(LivePttPlayback::is_active_or_buffered);
                    if !input_gate.allows_input(
                        Instant::now(),
                        playback_active,
                        output_muted,
                    ) {
                        if orb_conversation_owns(&app, sequence) {
                            crate::orb_window::set_audio_level(&app, "input", 0.0);
                        }
                        continue;
                    }
                    ws_write
                        .send(Message::Binary(frame))
                        .await
                        .map_err(|error| format!("failed to send live PTT audio frame: {error}"))?;
                },
                else => {
                    return Err("live PTT control channels ended unexpectedly".to_string());
                },
            }
        }
    }
    .await;

    let disconnect_reason = if result.is_ok() {
        "client_closed"
    } else {
        "error"
    };
    post_live_ptt_session_event(
        &app,
        &session_id,
        MEDIA_VOICE_BRIDGE_DISCONNECTED,
        json!({
            "reason": disconnect_reason,
            "sequence": sequence,
        }),
    )
    .await
    .ok();
    if let Err(error) =
        disconnect_live_ptt_media_session(&app, &session_id, disconnect_reason).await
    {
        warn!("Failed to disconnect live PTT media session: {}", error);
    }
    result
}

fn live_ptt_control_clears_playback(text: &str) -> bool {
    serde_json::from_str::<VoiceControlEnvelope>(text)
        .map(|envelope| {
            matches!(
                envelope.kind.as_str(),
                "response.interrupted" | "audio.output.ended"
            ) && envelope
                .payload
                .get("interrupted")
                .and_then(Value::as_bool)
                .unwrap_or(envelope.kind == "response.interrupted")
        })
        .unwrap_or(false)
}

fn live_response_id(payload: &Value) -> Option<String> {
    payload
        .get("response_id")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("response")
                .and_then(|response| response.get("id"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
}

fn output_end_matches(active: Option<&str>, incoming: Option<&str>) -> bool {
    !matches!((active, incoming), (Some(active), Some(incoming)) if active != incoming)
}

async fn track_live_output_event(
    app: &AppHandle,
    sequence: u64,
    payload: &Value,
    started: bool,
) -> bool {
    let incoming = live_response_id(payload);
    let state = app.state::<crate::AppState>();
    let mut voice = state.voice_hotkeys.lock().await;
    let current_session = voice
        .live
        .active
        .as_ref()
        .is_some_and(|active| active.sequence == sequence);
    if !current_session {
        return false;
    }
    if started {
        voice.live.active_output_response_id = incoming;
        return true;
    }
    if !output_end_matches(
        voice.live.active_output_response_id.as_deref(),
        incoming.as_deref(),
    ) {
        return false;
    }
    voice.live.active_output_response_id = None;
    true
}

async fn send_ws_json<S>(sink: &mut S, kind: &str, payload: Value) -> Result<(), String>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    sink.send(Message::Text(
        json!({ "kind": kind, "payload": payload }).to_string(),
    ))
    .await
    .map_err(|error| format!("failed to send live PTT {kind}: {error}"))
}

fn live_ptt_error_is_recoverable(payload: &Value) -> bool {
    payload
        .get("recoverable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn assistant_name_from_session_ready(payload: &Value) -> Option<String> {
    payload
        .get("agent")
        .and_then(|agent| agent.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IgnoredTranscriptFeedback {
    Silent,
    Guidance(String),
    Failure(String),
}

fn ignored_transcript_feedback(payload: &Value) -> IgnoredTranscriptFeedback {
    let reason = payload
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match reason {
        // `self_echo` is an admission rejection, but telling the user that the
        // assistant heard itself is noisier than simply returning to listen.
        "self_echo"
        // Older backends used `transcript.user.ignored` for lifecycle cleanup.
        // Keep mixed-version desktop/backend pairs silent during upgrades.
        | "no_final_transcript"
        | "input_cleared"
        | "session_ended"
        | "local_transcript_disabled"
        | "local_transcript_failed"
        | "provider_commit_unavailable" => IgnoredTranscriptFeedback::Silent,
        "address_prefix_armed" => {
            IgnoredTranscriptFeedback::Guidance("Listening — go ahead.".to_string())
        },
        "address_prefix_required" => {
            let phrase = payload
                .get("activation_phrases")
                .and_then(Value::as_array)
                .and_then(|phrases| phrases.iter().find_map(Value::as_str))
                .map(str::trim)
                .filter(|phrase| !phrase.is_empty());
            IgnoredTranscriptFeedback::Guidance(match phrase {
                Some(phrase) => format!("Start with “{phrase}”."),
                None => "Start with the wake phrase.".to_string(),
            })
        },
        _ => IgnoredTranscriptFeedback::Failure("Voice input was not sent.".to_string()),
    }
}

async fn active_assistant_name(app: &AppHandle, sequence: u64) -> String {
    let state = app.state::<crate::AppState>();
    let voice = state.voice_hotkeys.lock().await;
    voice
        .live
        .active
        .as_ref()
        .filter(|active| active.sequence == sequence)
        .and_then(|active| active.assistant_name.clone())
        .unwrap_or_else(|| ASSISTANT_FALLBACK_NAME.to_string())
}

async fn handle_live_ptt_control_message(
    app: &AppHandle,
    session_id: &str,
    text: &str,
    sequence: u64,
    output_muted: bool,
) -> Result<LivePttControlOutcome, String> {
    let envelope: VoiceControlEnvelope = serde_json::from_str(text)
        .map_err(|error| format!("failed to parse live PTT control message: {error}"))?;
    let mut outcome = LivePttControlOutcome::default();
    match envelope.kind.as_str() {
        "session.ready" => {
            let topology = envelope
                .payload
                .get("descriptor")
                .and_then(|v| v.get("topology"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if topology != "backend_proxied" {
                return Err(format!(
                    "live PTT requires a backend_proxied realtime profile; backend returned {topology}"
                ));
            }
            let assistant_name = assistant_name_from_session_ready(&envelope.payload);
            {
                let state = app.state::<crate::AppState>();
                let mut voice = state.voice_hotkeys.lock().await;
                if let Some(active) = voice
                    .live
                    .active
                    .as_mut()
                    .filter(|active| active.sequence == sequence)
                {
                    active.assistant_name = assistant_name;
                }
            }
            emit_voice_note_state(app, "live_ptt_ready", "Live PTT ready.", Some(sequence));
            if orb_conversation_owns(app, sequence) {
                crate::orb_window::dispatch(app, crate::orb_state::OrbAction::Connected);
            }
            arm_orb_streaming_idle_timeout(app, sequence).await;
            post_live_ptt_session_event(
                app,
                session_id,
                MEDIA_VOICE_BRIDGE_CONNECTED,
                json!({
                    "topology": topology,
                    "sequence": sequence,
                }),
            )
            .await
            .ok();
        },
        "speech.started" => {
            set_orb_streaming_speech_active(app, sequence, true).await;
            emit_voice_note_state(
                app,
                "live_ptt_speech_started",
                "Speech detected.",
                Some(sequence),
            );
            let _ = set_voice_visual(app, "listening").await;
            if orb_conversation_owns(app, sequence) {
                crate::orb_window::dispatch(
                    app,
                    crate::orb_state::OrbAction::Turn(crate::orb_state::OrbTurn::Listening),
                );
            }
        },
        "speech.stopped" => {
            set_orb_streaming_speech_active(app, sequence, false).await;
            emit_voice_note_state(
                app,
                "live_ptt_speech_stopped",
                "Speech stopped.",
                Some(sequence),
            );
            let _ = set_voice_visual(app, "thinking").await;
            if orb_conversation_owns(app, sequence) {
                crate::orb_window::dispatch(
                    app,
                    crate::orb_state::OrbAction::Turn(crate::orb_state::OrbTurn::Thinking),
                );
            }
        },
        "transcript.user" | "transcript.user.partial" => {
            if let Some(text) = envelope.payload.get("text").and_then(|v| v.as_str()) {
                if orb_conversation_owns(app, sequence) {
                    crate::orb_window::emit_caption(
                        app,
                        "user",
                        "You",
                        text,
                        envelope.kind == "transcript.user",
                    );
                }
                let _ = show_voice_bubble(app, &format!("You: {}", truncate_for_bubble(text, 160)))
                    .await;
            }
        },
        "transcript.user.cleared" => {
            let reason = envelope
                .payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unspecified");
            info!(sequence, reason, "cleared unfinished live voice transcript");
            emit_voice_note_state(
                app,
                "live_ptt_transcript_cleared",
                "Listening for the next turn.",
                Some(sequence),
            );
            if orb_conversation_owns(app, sequence) {
                crate::orb_window::clear_unfinished_caption(app, "user");
                settle_orb_after_streaming_quiet(app, sequence).await;
            } else {
                let _ = set_voice_visual(app, "listening").await;
            }
        },
        "transcript.user.ignored" => {
            let reason = envelope
                .payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unspecified");
            let feedback = ignored_transcript_feedback(&envelope.payload);
            info!(
                sequence,
                reason,
                ?feedback,
                "live voice transcript rejected"
            );
            if orb_conversation_owns(app, sequence) {
                crate::orb_window::clear_unfinished_caption(app, "user");
            }
            let _ = set_voice_visual(app, "listening").await;
            match feedback {
                IgnoredTranscriptFeedback::Silent => {
                    emit_voice_note_state(
                        app,
                        "live_ptt_transcript_ignored",
                        "Listening for the next turn.",
                        Some(sequence),
                    );
                },
                IgnoredTranscriptFeedback::Guidance(message) => {
                    emit_voice_note_state(
                        app,
                        "live_ptt_transcript_ignored",
                        &message,
                        Some(sequence),
                    );
                    if orb_conversation_owns(app, sequence) {
                        let assistant_name = active_assistant_name(app, sequence).await;
                        crate::orb_window::emit_caption(
                            app,
                            "assistant",
                            &assistant_name,
                            &message,
                            true,
                        );
                    }
                    let _ = show_voice_bubble(app, &message).await;
                },
                IgnoredTranscriptFeedback::Failure(message) => {
                    emit_voice_note_state(
                        app,
                        "live_ptt_transcript_ignored",
                        &message,
                        Some(sequence),
                    );
                    if orb_conversation_owns(app, sequence) {
                        let assistant_name = active_assistant_name(app, sequence).await;
                        crate::orb_window::emit_caption(
                            app,
                            "assistant",
                            &assistant_name,
                            &message,
                            true,
                        );
                    }
                    let _ = show_voice_bubble(app, &message).await;
                },
            }
            settle_orb_after_streaming_quiet(app, sequence).await;
        },
        "transcript.assistant" | "transcript.assistant.delta" => {
            if let Some(text) = envelope.payload.get("text").and_then(|v| v.as_str()) {
                let assistant_name = active_assistant_name(app, sequence).await;
                let _ = set_voice_visual(app, "speaking").await;
                if orb_conversation_owns(app, sequence) {
                    crate::orb_window::emit_caption(
                        app,
                        "assistant",
                        &assistant_name,
                        text,
                        assistant_caption_final(&envelope.kind).unwrap_or(false),
                    );
                }
                let _ = show_voice_bubble(
                    app,
                    &format!("{assistant_name}: {}", truncate_for_bubble(text, 180)),
                )
                .await;
            }
        },
        "session.error" => {
            let message = envelope
                .payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("live PTT session error");
            let recoverable = live_ptt_error_is_recoverable(&envelope.payload);
            post_live_ptt_session_event(
                app,
                session_id,
                MEDIA_VOICE_BRIDGE_ERROR,
                json!({
                    "stage": "provider",
                    "error": message,
                    "recoverable": recoverable,
                    "sequence": sequence,
                }),
            )
            .await
            .ok();
            if recoverable {
                emit_voice_note_state(app, "live_ptt_degraded", message, Some(sequence));
                if orb_conversation_owns(app, sequence) {
                    crate::orb_window::dispatch(
                        app,
                        crate::orb_state::OrbAction::RecoverableError {
                            message: message.to_string(),
                        },
                    );
                }
                let _ = show_voice_bubble(app, message).await;
                return Ok(outcome);
            }
            return Err(message.to_string());
        },
        "audio.output.started" => {
            if !track_live_output_event(app, sequence, &envelope.payload, true).await {
                return Ok(outcome);
            }
            invalidate_orb_streaming_idle_timeout(app, sequence).await;
            if orb_conversation_owns(app, sequence) {
                crate::orb_window::dispatch(
                    app,
                    crate::orb_state::OrbAction::Turn(if output_muted {
                        crate::orb_state::OrbTurn::Thinking
                    } else {
                        crate::orb_state::OrbTurn::Speaking
                    }),
                );
            }
            outcome.begin_playback_segment = !output_muted;
        },
        "audio.output.ended" | "response.interrupted" => {
            if !track_live_output_event(app, sequence, &envelope.payload, false).await {
                return Ok(outcome);
            }
            outcome.clear_playback = live_ptt_control_clears_playback(text);
            outcome.end_playback_segment = !outcome.clear_playback && !output_muted;
            if orb_conversation_owns(app, sequence) {
                crate::orb_window::set_audio_level(app, "output", 0.0);
            }
            settle_orb_after_streaming_quiet(app, sequence).await;
        },
        "session.rotating" => {
            emit_voice_note_state(
                app,
                "live_ptt_rotating",
                "Live PTT refreshing realtime session...",
                Some(sequence),
            );
            post_live_ptt_session_event(
                app,
                session_id,
                MEDIA_VOICE_CONTROLLER_COMMAND,
                json!({
                    "command": "session.rotating",
                    "reason": envelope.payload.get("reason").cloned().unwrap_or(Value::Null),
                    "sequence": sequence,
                }),
            )
            .await
            .ok();
        },
        "audio.rebind" => {
            emit_voice_note_state(
                app,
                "live_ptt_rebound",
                "Live PTT realtime session refreshed.",
                Some(sequence),
            );
            post_live_ptt_session_event(
                app,
                session_id,
                MEDIA_VOICE_CONTROLLER_COMMAND,
                json!({
                    "command": "audio.rebind",
                    "rotation_count": envelope
                        .payload
                        .get("rotation_count")
                        .cloned()
                        .unwrap_or(Value::Null),
                    "sequence": sequence,
                }),
            )
            .await
            .ok();
        },
        "session.ended" => {
            emit_voice_note_state(app, "live_ptt_ended", "Live PTT ended.", Some(sequence));
            // Ownership is released by the control-loop epilogue after the
            // capture has been dropped, so cooldown wake cannot race the mic.
            outcome.keep_open = false;
        },
        _ => {},
    }
    Ok(outcome)
}

fn truncate_for_bubble(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() > max_chars {
        format!("{}...", trimmed.chars().take(max_chars).collect::<String>())
    } else {
        trimmed.to_string()
    }
}

impl LivePttCapture {
    fn start(
        app: AppHandle,
        audio_tx: tokio_mpsc::Sender<Vec<u8>>,
        sequence: u64,
    ) -> Result<Self, String> {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<LivePttCaptureReady, String>>(1);
        let join_handle = std::thread::Builder::new()
            .name("magician-live-ptt-capture".to_string())
            .spawn(move || run_live_ptt_capture_thread(app, audio_tx, sequence, stop_rx, ready_tx))
            .map_err(|error| format!("failed to spawn live PTT capture thread: {error}"))?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(ready)) => {
                drop(join_handle);
                Ok(Self {
                    stop_tx,
                    samples_sent: ready.samples_sent,
                    input_label: ready.input_label,
                })
            },
            Ok(Err(error)) => {
                let _ = join_handle.join();
                Err(error)
            },
            Err(error) => {
                let _ = stop_tx.send(());
                let _ = join_handle.join();
                Err(format!(
                    "timed out waiting for live PTT microphone to start: {error}"
                ))
            },
        }
    }
}

fn run_live_ptt_capture_thread(
    app: AppHandle,
    audio_tx: tokio_mpsc::Sender<Vec<u8>>,
    sequence: u64,
    stop_rx: mpsc::Receiver<()>,
    ready_tx: mpsc::SyncSender<Result<LivePttCaptureReady, String>>,
) {
    let (runtime_error_tx, runtime_error_rx) = mpsc::channel::<String>();
    let stream = match LivePttInputStream::start(app.clone(), audio_tx, sequence, runtime_error_tx)
    {
        Ok(stream) => {
            let _ = ready_tx.send(Ok(LivePttCaptureReady {
                samples_sent: Arc::clone(&stream.samples_sent),
                input_label: stream.input_label.clone(),
            }));
            stream
        },
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        },
    };
    let mut runtime_error = None;
    loop {
        if let Ok(error) = runtime_error_rx.try_recv() {
            runtime_error = Some(error);
            break;
        }
        match stop_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {},
        }
    }
    drop(stream);
    if let Some(error) = runtime_error {
        let failure_app = app.clone();
        tauri::async_runtime::spawn(async move {
            warn!("Live PTT microphone failed: {error}");
            emit_voice_note_state(&failure_app, "live_ptt_error", &error, Some(sequence));
            let _ = set_voice_visual(&failure_app, "failed").await;
            let _ = show_voice_bubble(&failure_app, &error).await;
            if orb_conversation_owns(&failure_app, sequence) {
                crate::orb_window::dispatch(
                    &failure_app,
                    crate::orb_state::OrbAction::RecoverableError {
                        message: error.clone(),
                    },
                );
                crate::orb_window::dispatch(
                    &failure_app,
                    crate::orb_state::OrbAction::Disarm {
                        reason: crate::orb_state::OrbEndedReason::MicrophoneLost,
                    },
                );
            } else {
                end_live_ptt_session_for_sequence(&failure_app, sequence).await;
            }
        });
    }
}

impl LivePttInputStream {
    fn start(
        app: AppHandle,
        audio_tx: tokio_mpsc::Sender<Vec<u8>>,
        sequence: u64,
        runtime_error_tx: mpsc::Sender<String>,
    ) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no default input device is available".to_string())?;
        let input_label = device
            .name()
            .unwrap_or_else(|_| "default microphone".to_string());
        let supported_config = device
            .default_input_config()
            .map_err(|error| format!("failed to read default input config: {error}"))?;
        let sample_format = supported_config.sample_format();
        let config: cpal::StreamConfig = supported_config.into();
        let samples_sent = Arc::new(AtomicU64::new(0));
        let orb_meter = orb_conversation_owns(&app, sequence)
            .then(|| Arc::clone(&app.state::<crate::AppState>().orb_input_level));
        let stream = match sample_format {
            cpal::SampleFormat::F32 => build_live_input_stream_f32(
                &device,
                &config,
                orb_meter.clone(),
                audio_tx,
                Arc::clone(&samples_sent),
                runtime_error_tx.clone(),
            )?,
            cpal::SampleFormat::I16 => build_live_input_stream_i16(
                &device,
                &config,
                orb_meter.clone(),
                audio_tx,
                Arc::clone(&samples_sent),
                runtime_error_tx.clone(),
            )?,
            cpal::SampleFormat::U16 => build_live_input_stream_u16(
                &device,
                &config,
                orb_meter,
                audio_tx,
                Arc::clone(&samples_sent),
                runtime_error_tx,
            )?,
            other => {
                return Err(format!(
                    "unsupported microphone sample format for live PTT: {other:?}"
                ));
            },
        };
        stream
            .play()
            .map_err(|error| format!("failed to start live PTT microphone stream: {error}"))?;
        Ok(Self {
            _stream: stream,
            samples_sent,
            input_label,
        })
    }
}

struct LivePttPlayback {
    command_tx: mpsc::Sender<LivePttPlaybackCommand>,
    stop_tx: mpsc::Sender<()>,
    queue: SharedPlaybackQueue,
}

impl Drop for LivePttPlayback {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
    }
}

enum LivePttPlaybackCommand {
    BeginSegment,
    Audio(Vec<u8>, Option<mpsc::SyncSender<()>>),
    EndSegment(Option<mpsc::SyncSender<()>>),
    Clear,
}

#[derive(Debug)]
struct PlaybackQueueState {
    samples: VecDeque<i16>,
    sample_rate_hz: u32,
    prebuffer_samples: usize,
    max_samples: usize,
    segment_active: bool,
    playing: bool,
    started_in_segment: bool,
    underrun_events: u64,
    underrun_samples: u64,
    dropped_samples: u64,
}

impl PlaybackQueueState {
    fn new(sample_rate_hz: u32) -> Self {
        let sample_rate_hz = sample_rate_hz.max(1);
        Self {
            samples: VecDeque::new(),
            sample_rate_hz,
            prebuffer_samples: samples_for_ms(sample_rate_hz, LIVE_PTT_PLAYBACK_PREBUFFER_MS),
            max_samples: samples_for_ms(sample_rate_hz, LIVE_PTT_PLAYBACK_MAX_BUFFER_MS),
            segment_active: false,
            playing: false,
            started_in_segment: false,
            underrun_events: 0,
            underrun_samples: 0,
            dropped_samples: 0,
        }
    }

    fn begin_segment(&mut self) {
        self.segment_active = true;
        // A provider can start its next response while the device is still
        // draining the prior response's tail. Preserve that already-buffered
        // playback instead of inserting a new prebuffer pause mid-sentence.
        if self.samples.is_empty() {
            self.playing = false;
            self.started_in_segment = false;
        } else {
            self.started_in_segment = true;
        }
        self.underrun_events = 0;
        self.underrun_samples = 0;
        self.dropped_samples = 0;
    }

    fn end_segment(&mut self) {
        self.segment_active = false;
        if !self.samples.is_empty() {
            // A short final response may never reach the prebuffer threshold.
            // Once the producer declares the segment complete, play its tail.
            self.playing = true;
            self.started_in_segment = true;
        }
    }

    fn clear(&mut self) {
        self.samples.clear();
        self.segment_active = false;
        self.playing = false;
        self.started_in_segment = false;
    }

    fn push_samples(&mut self, samples: impl IntoIterator<Item = i16>) {
        for sample in samples {
            if self.samples.len() >= self.max_samples {
                let _ = self.samples.pop_front();
                self.dropped_samples = self.dropped_samples.saturating_add(1);
            }
            self.samples.push_back(sample);
        }
    }

    fn pop_sample(&mut self) -> i16 {
        if !self.playing {
            let can_start = !self.samples.is_empty()
                && (!self.segment_active || self.samples.len() >= self.prebuffer_samples);
            if can_start {
                self.playing = true;
                self.started_in_segment = true;
            }
        }
        if self.playing {
            if let Some(sample) = self.samples.pop_front() {
                return sample;
            }
            self.playing = false;
            if self.segment_active && self.started_in_segment {
                self.underrun_events = self.underrun_events.saturating_add(1);
            }
        }
        if self.segment_active && self.started_in_segment {
            self.underrun_samples = self.underrun_samples.saturating_add(1);
        }
        0
    }

    fn queued_ms(&self) -> u64 {
        (self.samples.len() as u64)
            .saturating_mul(1_000)
            .checked_div(u64::from(self.sample_rate_hz))
            .unwrap_or(0)
    }

    fn drained(&self) -> bool {
        self.samples.is_empty() && !self.playing
    }
}

fn samples_for_ms(sample_rate_hz: u32, duration_ms: u64) -> usize {
    u64::from(sample_rate_hz)
        .saturating_mul(duration_ms)
        .saturating_add(999)
        .checked_div(1_000)
        .unwrap_or(1)
        .max(1) as usize
}

struct LivePttOutputStream {
    _stream: cpal::Stream,
    queue: SharedPlaybackQueue,
    resampler: StdMutex<Pcm16Resampler>,
}

impl LivePttPlayback {
    fn start() -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::channel::<LivePttPlaybackCommand>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<SharedPlaybackQueue, String>>(1);
        let join_handle = std::thread::Builder::new()
            .name("magician-live-ptt-playback".to_string())
            .spawn(move || run_live_ptt_playback_thread(command_rx, stop_rx, ready_tx))
            .map_err(|error| format!("failed to spawn live PTT playback thread: {error}"))?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(queue)) => {
                drop(join_handle);
                Ok(Self {
                    command_tx,
                    stop_tx,
                    queue,
                })
            },
            Ok(Err(error)) => {
                let _ = join_handle.join();
                Err(error)
            },
            Err(error) => {
                let _ = stop_tx.send(());
                let _ = join_handle.join();
                Err(format!(
                    "timed out waiting for live PTT playback to start: {error}"
                ))
            },
        }
    }

    fn push_pcm16_bytes(&self, bytes: &[u8]) {
        let _ = self
            .command_tx
            .send(LivePttPlaybackCommand::Audio(bytes.to_vec(), None));
    }

    fn push_pcm16_bytes_paced(&self, bytes: &[u8]) {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if self
            .command_tx
            .send(LivePttPlaybackCommand::Audio(bytes.to_vec(), Some(ack_tx)))
            .is_ok()
        {
            let _ = ack_rx.recv_timeout(Duration::from_secs(1));
        }
    }

    fn begin_segment(&self) {
        let _ = self.command_tx.send(LivePttPlaybackCommand::BeginSegment);
    }

    fn end_segment(&self) {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if self
            .command_tx
            .send(LivePttPlaybackCommand::EndSegment(Some(ack_tx)))
            .is_ok()
        {
            let _ = ack_rx.recv_timeout(Duration::from_secs(1));
        }
    }

    fn end_streaming_segment(&self) {
        let _ = self
            .command_tx
            .send(LivePttPlaybackCommand::EndSegment(None));
    }

    fn queued_ms(&self) -> u64 {
        self.queue
            .lock()
            .map(|queue| queue.queued_ms())
            .unwrap_or_default()
    }

    fn is_active_or_buffered(&self) -> bool {
        self.queue
            .lock()
            .map(|queue| queue.segment_active || queue.playing || !queue.samples.is_empty())
            // A poisoned playback queue is not evidence that capture is safe;
            // fail closed so speaker audio cannot be promoted as user speech.
            .unwrap_or(true)
    }

    fn wait_for_producer_room(&self, keep_playing: &dyn Fn() -> bool) -> bool {
        if self.queued_ms() < LIVE_PTT_PLAYBACK_PRODUCER_HIGH_WATER_MS {
            return keep_playing();
        }
        while self.queued_ms() > LIVE_PTT_PLAYBACK_PRODUCER_LOW_WATER_MS {
            if !keep_playing() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    fn wait_until_drained(&self, keep_playing: &dyn Fn() -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if !keep_playing() {
                return false;
            }
            if self
                .queue
                .lock()
                .map(|queue| queue.drained())
                .unwrap_or(true)
            {
                return true;
            }
            if Instant::now() >= deadline {
                warn!(
                    queued_ms = self.queued_ms(),
                    "Timed out waiting for native voice playback to drain"
                );
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn clear(&self) {
        let _ = self.command_tx.send(LivePttPlaybackCommand::Clear);
    }
}

fn run_live_ptt_playback_thread(
    command_rx: mpsc::Receiver<LivePttPlaybackCommand>,
    stop_rx: mpsc::Receiver<()>,
    ready_tx: mpsc::SyncSender<Result<SharedPlaybackQueue, String>>,
) {
    let playback = match LivePttOutputStream::start() {
        Ok(playback) => {
            let _ = ready_tx.send(Ok(Arc::clone(&playback.queue)));
            playback
        },
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        },
    };

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        match command_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(LivePttPlaybackCommand::BeginSegment) => playback.begin_segment(),
            Ok(LivePttPlaybackCommand::Audio(bytes, ack)) => {
                playback.push_pcm16_bytes(&bytes);
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            },
            Ok(LivePttPlaybackCommand::EndSegment(ack)) => {
                playback.end_segment();
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            },
            Ok(LivePttPlaybackCommand::Clear) => playback.clear(),
            Err(mpsc::RecvTimeoutError::Timeout) => {},
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(playback);
}

impl LivePttOutputStream {
    fn start() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default output device is available".to_string())?;
        let supported_config = device
            .default_output_config()
            .map_err(|error| format!("failed to read default output config: {error}"))?;
        let sample_format = supported_config.sample_format();
        let config: cpal::StreamConfig = supported_config.into();
        let output_sample_rate = config.sample_rate.0;
        let output_channels = config.channels.max(1) as usize;
        let queue = Arc::new(StdMutex::new(PlaybackQueueState::new(output_sample_rate)));
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                build_live_output_stream_f32(&device, &config, Arc::clone(&queue), output_channels)?
            },
            cpal::SampleFormat::I16 => {
                build_live_output_stream_i16(&device, &config, Arc::clone(&queue), output_channels)?
            },
            cpal::SampleFormat::U16 => {
                build_live_output_stream_u16(&device, &config, Arc::clone(&queue), output_channels)?
            },
            other => {
                return Err(format!(
                    "unsupported output sample format for live PTT: {other:?}"
                ))
            },
        };
        stream
            .play()
            .map_err(|error| format!("failed to start live PTT output stream: {error}"))?;
        Ok(Self {
            _stream: stream,
            queue,
            resampler: StdMutex::new(Pcm16Resampler::new(
                LIVE_PTT_WIRE_SAMPLE_RATE_HZ,
                output_sample_rate,
            )),
        })
    }

    fn push_pcm16_bytes(&self, bytes: &[u8]) {
        let mut samples = Vec::with_capacity(bytes.len() / 2);
        for chunk in bytes.chunks_exact(2) {
            samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
        }
        let samples = match self.resampler.lock() {
            Ok(mut resampler) => resampler.push(&samples),
            Err(_) => samples,
        };
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        queue.push_samples(samples);
    }

    fn begin_segment(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.begin_segment();
        }
    }

    fn end_segment(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.end_segment();
            if queue.underrun_events > 0 || queue.dropped_samples > 0 {
                warn!(
                    underrun_events = queue.underrun_events,
                    underrun_ms = queue.underrun_samples.saturating_mul(1_000)
                        / u64::from(queue.sample_rate_hz),
                    dropped_samples = queue.dropped_samples,
                    queued_ms = queue.queued_ms(),
                    "Native voice playback recovered from producer jitter"
                );
            }
        }
    }

    fn clear(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.clear();
        }
    }
}

impl WavRecorder {
    fn start(level_meter: Option<Arc<AtomicU32>>) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no default input device is available".to_string())?;
        let input_label = device
            .name()
            .unwrap_or_else(|_| "default microphone".to_string());
        let supported_config = device
            .default_input_config()
            .map_err(|error| format!("failed to read default input config: {error}"))?;
        let sample_format = supported_config.sample_format();
        let config: cpal::StreamConfig = supported_config.into();
        let path = voice_note_temp_path()?;
        let spec = WavSpec {
            channels: config.channels,
            sample_rate: config.sample_rate.0,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let writer = WavWriter::create(&path, spec)
            .map_err(|error| format!("failed to create voice note wav file: {error}"))?;
        let writer = Arc::new(StdMutex::new(Some(writer)));
        let samples_written = Arc::new(AtomicU64::new(0));

        let stream = match sample_format {
            cpal::SampleFormat::F32 => build_input_stream_f32(
                &device,
                &config,
                Arc::clone(&writer),
                Arc::clone(&samples_written),
                level_meter.clone(),
            )?,
            cpal::SampleFormat::I16 => build_input_stream_i16(
                &device,
                &config,
                Arc::clone(&writer),
                Arc::clone(&samples_written),
                level_meter.clone(),
            )?,
            cpal::SampleFormat::U16 => build_input_stream_u16(
                &device,
                &config,
                Arc::clone(&writer),
                Arc::clone(&samples_written),
                level_meter,
            )?,
            other => {
                return Err(format!(
                    "unsupported microphone sample format for voice notes: {other:?}"
                ));
            },
        };
        stream
            .play()
            .map_err(|error| format!("failed to start microphone stream: {error}"))?;

        Ok(Self {
            stream,
            writer,
            path,
            samples_written,
            input_label,
        })
    }

    fn stop(self) -> Result<CompletedVoiceNote, String> {
        drop(self.stream);
        let samples_written = self.samples_written.load(Ordering::Relaxed);
        {
            let mut writer_guard = self
                .writer
                .lock()
                .map_err(|_| "voice note writer lock poisoned".to_string())?;
            let Some(writer) = writer_guard.take() else {
                return Err("voice note writer was already finalized".to_string());
            };
            writer
                .finalize()
                .map_err(|error| format!("failed to finalize voice note wav: {error}"))?;
        }
        let native_bytes = fs::read(&self.path)
            .map_err(|error| format!("failed to read voice note wav: {error}"))?;
        if let Err(error) = fs::remove_file(&self.path) {
            warn!(
                "Failed to remove temporary voice note file {}: {}",
                self.path.display(),
                error
            );
        }
        // The note exists to be transcribed, and it is uploaded twice on the
        // way (to the backend, then to the STT vendor); the microphone's
        // native stream is 96 kHz on this Mac, six times the bytes of what
        // transcription uses. A conversion failure keeps the native bytes:
        // a slower note beats a lost one.
        let bytes = match downsample_wav_for_transcription(&native_bytes) {
            Ok(bytes) => bytes,
            Err(error) => {
                warn!(
                    native_bytes = native_bytes.len(),
                    %error,
                    "voice note kept at its native sample rate"
                );
                native_bytes
            },
        };
        Ok(CompletedVoiceNote {
            bytes,
            samples_written,
            input_label: self.input_label,
        })
    }
}

struct RecordingThread {
    stop_tx: mpsc::Sender<()>,
    join_handle: std::thread::JoinHandle<Result<CompletedVoiceNote, String>>,
}

fn spawn_recording_thread() -> Result<RecordingThread, String> {
    spawn_recording_thread_with_meter(None)
}

fn spawn_recording_thread_with_meter(
    level_meter: Option<Arc<AtomicU32>>,
) -> Result<RecordingThread, String> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let join_handle = std::thread::Builder::new()
        .name("magician-voice-note-recorder".to_string())
        .spawn(move || run_recording_thread(stop_rx, ready_tx, level_meter))
        .map_err(|error| format!("failed to spawn voice note recording thread: {error}"))?;

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(RecordingThread {
            stop_tx,
            join_handle,
        }),
        Ok(Err(error)) => {
            let _ = join_handle.join();
            Err(error)
        },
        Err(error) => {
            let _ = stop_tx.send(());
            let _ = join_handle.join();
            Err(format!(
                "timed out waiting for microphone recording to start: {error}"
            ))
        },
    }
}

fn run_recording_thread(
    stop_rx: mpsc::Receiver<()>,
    ready_tx: mpsc::SyncSender<Result<(), String>>,
    level_meter: Option<Arc<AtomicU32>>,
) -> Result<CompletedVoiceNote, String> {
    let recorder = match WavRecorder::start(level_meter) {
        Ok(recorder) => {
            let _ = ready_tx.send(Ok(()));
            recorder
        },
        Err(error) => {
            let _ = ready_tx.send(Err(error.clone()));
            return Err(error);
        },
    };
    let _ = stop_rx.recv();
    recorder.stop()
}

fn record_voice_note_for(duration: Duration) -> Result<CompletedVoiceNote, String> {
    let recording = spawn_recording_thread()?;
    std::thread::sleep(duration);
    let _ = recording.stop_tx.send(());
    recording
        .join_handle
        .join()
        .map_err(|_| "voice note recording test thread panicked".to_string())?
}

fn play_voice_test_wav(audio_bytes: &[u8]) -> Result<u128, String> {
    let (playback_bytes, playback_duration) = wav_bytes_to_live_ptt_pcm16(audio_bytes)?;
    if playback_bytes.is_empty() {
        return Err("voice recording test produced no playback samples".to_string());
    }
    let playback = LivePttPlayback::start()?;
    playback.push_pcm16_bytes(&playback_bytes);
    std::thread::sleep(playback_duration + VOICE_NOTE_TEST_PLAYBACK_PAD);
    drop(playback);
    Ok(playback_duration.as_millis())
}

/// Sample rate voice notes are uploaded at. Speech-to-text models work on
/// 16 kHz mono; anything above it is bytes, not words.
const VOICE_NOTE_UPLOAD_SAMPLE_RATE_HZ: u32 = 16_000;

/// Re-encode a recorded WAV as 16 kHz mono PCM16 for transcription. Returns
/// the input unchanged when it is already at or below that rate and mono.
fn downsample_wav_for_transcription(audio_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut reader = WavReader::new(Cursor::new(audio_bytes))
        .map_err(|error| format!("failed to read recorded wav: {error}"))?;
    let spec = reader.spec();
    if spec.sample_rate <= VOICE_NOTE_UPLOAD_SAMPLE_RATE_HZ && spec.channels == 1 {
        return Ok(audio_bytes.to_vec());
    }
    let channels = spec.channels.max(1) as usize;
    let mono_samples = match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Int, 16) => {
            let samples = reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("failed to decode recorded PCM16 wav: {error}"))?;
            if channels == 1 {
                samples
            } else {
                pcm_i16_to_mono_i16_samples(&samples, channels)
            }
        },
        (SampleFormat::Float, 32) => {
            let samples = reader
                .samples::<f32>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("failed to decode recorded Float32 wav: {error}"))?;
            pcm_f32_to_mono_i16_samples(&samples, channels)
        },
        (sample_format, bits_per_sample) => {
            return Err(format!(
                "unsupported recorded wav format: {sample_format:?} {bits_per_sample}-bit"
            ));
        },
    };
    let target_rate = VOICE_NOTE_UPLOAD_SAMPLE_RATE_HZ.min(spec.sample_rate.max(1));
    let mut resampler = Pcm16Resampler::new(spec.sample_rate.max(1), target_rate);
    let resampled = resampler.push(&mono_samples);
    let mut cursor = Cursor::new(Vec::with_capacity(44 + resampled.len() * 2));
    {
        let mut writer = WavWriter::new(
            &mut cursor,
            WavSpec {
                channels: 1,
                sample_rate: target_rate,
                bits_per_sample: 16,
                sample_format: SampleFormat::Int,
            },
        )
        .map_err(|error| format!("failed to start transcription wav: {error}"))?;
        for sample in resampled {
            writer
                .write_sample(sample)
                .map_err(|error| format!("failed to write transcription wav: {error}"))?;
        }
        writer
            .finalize()
            .map_err(|error| format!("failed to finalize transcription wav: {error}"))?;
    }
    Ok(cursor.into_inner())
}

fn wav_bytes_to_live_ptt_pcm16(audio_bytes: &[u8]) -> Result<(Vec<u8>, Duration), String> {
    wav_bytes_to_live_ptt_pcm16_from_source(audio_bytes, "recorded")
}

fn wav_bytes_to_live_ptt_pcm16_from_source(
    audio_bytes: &[u8],
    source: &str,
) -> Result<(Vec<u8>, Duration), String> {
    let (audio_bytes, discarded_tail_bytes, normalized_streaming_length) =
        align_supported_wav_data_chunk(audio_bytes);
    if discarded_tail_bytes > 0 || normalized_streaming_length {
        warn!(
            source,
            discarded_tail_bytes,
            normalized_streaming_length,
            "Canonicalized WAV data length for native playback"
        );
    }
    let cursor = Cursor::new(audio_bytes.as_ref());
    let mut reader =
        WavReader::new(cursor).map_err(|error| format!("failed to read {source} wav: {error}"))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let mono_samples = match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Int, 16) => {
            let samples = reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("failed to decode {source} PCM16 wav samples: {error}"))?;
            if channels == 1 {
                samples
            } else {
                pcm_i16_to_mono_i16_samples(&samples, channels)
            }
        },
        (SampleFormat::Float, 32) => {
            let samples = reader
                .samples::<f32>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    format!("failed to decode {source} Float32 wav samples: {error}")
                })?;
            pcm_f32_to_mono_i16_samples(&samples, channels)
        },
        (sample_format, bits_per_sample) => {
            return Err(format!(
                "unsupported {source} wav format: {sample_format:?} {bits_per_sample}-bit"
            ));
        },
    };
    if mono_samples.is_empty() {
        return Err(format!("{source} wav contains no audio samples"));
    }
    let source_rate = spec.sample_rate.max(1);
    let playback_duration = Duration::from_secs_f64(mono_samples.len() as f64 / source_rate as f64);
    let mut resampler = Pcm16Resampler::new(source_rate, LIVE_PTT_WIRE_SAMPLE_RATE_HZ);
    let playback_samples = resampler.push(&mono_samples);
    Ok((
        pcm_i16_samples_to_le_bytes(&playback_samples),
        playback_duration,
    ))
}

/// Some WAV producers declare an unknown streaming data length (`u32::MAX`) or
/// one final, incomplete sample frame. By playback time the response body is
/// fully collected, so canonicalize only that explicit streaming sentinel or
/// reduce a present data chunk to the last complete supported PCM16/Float32
/// frame. Other oversized declarations remain errors. The RIFF scan is bounded,
/// iterative, and monotonically advancing.
fn align_supported_wav_data_chunk(audio_bytes: &[u8]) -> (Cow<'_, [u8]>, usize, bool) {
    if audio_bytes.len() < 12 || &audio_bytes[0..4] != b"RIFF" || &audio_bytes[8..12] != b"WAVE" {
        return (Cow::Borrowed(audio_bytes), 0, false);
    }

    let mut offset = 12_usize;
    let mut frame_bytes = None;
    while let Some(header_end) = offset.checked_add(8) {
        if header_end > audio_bytes.len() {
            break;
        }
        let chunk_len = u32::from_le_bytes([
            audio_bytes[offset + 4],
            audio_bytes[offset + 5],
            audio_bytes[offset + 6],
            audio_bytes[offset + 7],
        ]) as usize;
        let data_start = header_end;
        let is_data_chunk = &audio_bytes[offset..offset + 4] == b"data";
        let streaming_length = is_data_chunk && chunk_len == u32::MAX as usize;
        let effective_chunk_len = if streaming_length {
            audio_bytes.len() - data_start
        } else {
            chunk_len
        };
        let Some(data_end) = data_start.checked_add(effective_chunk_len) else {
            break;
        };
        if data_end > audio_bytes.len() {
            break;
        }

        match &audio_bytes[offset..offset + 4] {
            b"fmt " if chunk_len >= 16 => {
                let audio_format =
                    u16::from_le_bytes([audio_bytes[data_start], audio_bytes[data_start + 1]]);
                let channels =
                    u16::from_le_bytes([audio_bytes[data_start + 2], audio_bytes[data_start + 3]]);
                let block_align = u16::from_le_bytes([
                    audio_bytes[data_start + 12],
                    audio_bytes[data_start + 13],
                ]);
                let bits_per_sample = u16::from_le_bytes([
                    audio_bytes[data_start + 14],
                    audio_bytes[data_start + 15],
                ]);
                // WAVE_FORMAT_EXTENSIBLE stores the effective PCM/IEEE-float
                // format in the first word of its subformat GUID. Hound uses
                // this representation for Float32 output, so resolve it before
                // deciding whether this is one of the two formats we can
                // safely align. The chunk-length guards keep every read inside
                // the already validated `fmt ` payload.
                let effective_audio_format = if audio_format == 0xfffe && chunk_len >= 40 {
                    let extension_len = u16::from_le_bytes([
                        audio_bytes[data_start + 16],
                        audio_bytes[data_start + 17],
                    ]);
                    (extension_len >= 22).then(|| {
                        u16::from_le_bytes([
                            audio_bytes[data_start + 24],
                            audio_bytes[data_start + 25],
                        ])
                    })
                } else {
                    Some(audio_format)
                };
                let bytes_per_sample = match (effective_audio_format, bits_per_sample) {
                    (Some(1), 16) => Some(2_u16),
                    (Some(3), 32) => Some(4_u16),
                    _ => None,
                };
                let expected_frame_bytes =
                    bytes_per_sample.and_then(|bytes| channels.checked_mul(bytes));
                if channels > 0 && Some(block_align) == expected_frame_bytes {
                    frame_bytes = Some(block_align as usize);
                }
            },
            b"data" => {
                let Some(frame_bytes) = frame_bytes else {
                    return (Cow::Borrowed(audio_bytes), 0, false);
                };
                let remainder = effective_chunk_len % frame_bytes;
                if remainder == 0 && !streaming_length {
                    return (Cow::Borrowed(audio_bytes), 0, false);
                }
                let aligned_len = effective_chunk_len - remainder;
                let Ok(aligned_len) = u32::try_from(aligned_len) else {
                    return (Cow::Borrowed(audio_bytes), 0, false);
                };
                let mut repaired = audio_bytes.to_vec();
                repaired[offset + 4..offset + 8].copy_from_slice(&aligned_len.to_le_bytes());
                if streaming_length {
                    let Ok(riff_len) = u32::try_from(repaired.len().saturating_sub(8)) else {
                        return (Cow::Borrowed(audio_bytes), 0, false);
                    };
                    repaired[4..8].copy_from_slice(&riff_len.to_le_bytes());
                }
                return (Cow::Owned(repaired), remainder, streaming_length);
            },
            _ => {},
        }

        let padded_len = match chunk_len.checked_add(chunk_len & 1) {
            Some(value) => value,
            None => break,
        };
        let Some(next_offset) = data_start.checked_add(padded_len) else {
            break;
        };
        if next_offset <= offset {
            break;
        }
        offset = next_offset;
    }
    (Cow::Borrowed(audio_bytes), 0, false)
}

fn build_input_stream_f32(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    writer: SharedWavWriter,
    samples_written: Arc<AtomicU64>,
    level_meter: Option<Arc<AtomicU32>>,
) -> Result<cpal::Stream, String> {
    device
        .build_input_stream(
            config,
            move |data: &[f32], _| {
                if let Some(meter) = level_meter.as_ref() {
                    store_peak_level(meter, input_f32_rms(data));
                }
                write_f32_samples(data, &writer, &samples_written);
            },
            log_stream_error,
            None,
        )
        .map_err(|error| format!("failed to build f32 input stream: {error}"))
}

fn build_input_stream_i16(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    writer: SharedWavWriter,
    samples_written: Arc<AtomicU64>,
    level_meter: Option<Arc<AtomicU32>>,
) -> Result<cpal::Stream, String> {
    device
        .build_input_stream(
            config,
            move |data: &[i16], _| {
                if let Some(meter) = level_meter.as_ref() {
                    store_peak_level(meter, pcm16_rms(data));
                }
                write_i16_samples(data, &writer, &samples_written);
            },
            log_stream_error,
            None,
        )
        .map_err(|error| format!("failed to build i16 input stream: {error}"))
}

fn build_input_stream_u16(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    writer: SharedWavWriter,
    samples_written: Arc<AtomicU64>,
    level_meter: Option<Arc<AtomicU32>>,
) -> Result<cpal::Stream, String> {
    device
        .build_input_stream(
            config,
            move |data: &[u16], _| {
                if let Some(meter) = level_meter.as_ref() {
                    store_peak_level(meter, input_u16_rms(data));
                }
                write_u16_samples(data, &writer, &samples_written);
            },
            log_stream_error,
            None,
        )
        .map_err(|error| format!("failed to build u16 input stream: {error}"))
}

/// Root-mean-square audio energy with a small noise floor and a perceptual gain
/// suitable for animation. It is deliberately independent of packet size.
fn pcm16_rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let mean_square = samples
        .iter()
        .map(|sample| {
            let normalized = *sample as f64 / i16::MAX as f64;
            normalized * normalized
        })
        .sum::<f64>()
        / samples.len() as f64;
    let raw = mean_square.sqrt() as f32;
    ((raw - 0.006).max(0.0) * 5.5).clamp(0.0, 1.0)
}

fn input_f32_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum = samples.iter().fold(0.0_f64, |sum, sample| {
        let normalized = f64::from((*sample).clamp(-1.0, 1.0));
        sum + normalized * normalized
    });
    let raw = (sum / samples.len() as f64).sqrt() as f32;
    ((raw - 0.006).max(0.0) * 5.5).clamp(0.0, 1.0)
}

fn input_u16_rms(samples: &[u16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum = samples.iter().fold(0.0_f64, |sum, sample| {
        let normalized = (f64::from(*sample) - 32_768.0) / 32_768.0;
        sum + normalized * normalized
    });
    let raw = (sum / samples.len() as f64).sqrt() as f32;
    ((raw - 0.006).max(0.0) * 5.5).clamp(0.0, 1.0)
}

fn pcm16_le_bytes_rms(bytes: &[u8]) -> f32 {
    if bytes.len() < 2 {
        return 0.0;
    }
    let mut sum = 0.0_f64;
    let mut count = 0_usize;
    for chunk in bytes.chunks_exact(2) {
        let normalized = i16::from_le_bytes([chunk[0], chunk[1]]) as f64 / i16::MAX as f64;
        sum += normalized * normalized;
        count += 1;
    }
    let raw = (sum / count.max(1) as f64).sqrt() as f32;
    ((raw - 0.006).max(0.0) * 5.5).clamp(0.0, 1.0)
}

fn store_orb_input_level(meter: &AtomicU32, samples: &[i16]) {
    meter.store(pcm16_rms(samples).to_bits(), Ordering::Relaxed);
}

fn store_peak_level(meter: &AtomicU32, level: f32) {
    let level = level.clamp(0.0, 1.0).to_bits();
    let mut current = meter.load(Ordering::Relaxed);
    while level > current {
        match meter.compare_exchange_weak(current, level, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

fn build_live_input_stream_f32(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    orb_meter: Option<Arc<AtomicU32>>,
    audio_tx: tokio_mpsc::Sender<Vec<u8>>,
    samples_sent: Arc<AtomicU64>,
    runtime_error_tx: mpsc::Sender<String>,
) -> Result<cpal::Stream, String> {
    let channels = config.channels.max(1) as usize;
    let mut resampler = Pcm16Resampler::new(config.sample_rate.0, LIVE_PTT_WIRE_SAMPLE_RATE_HZ);
    device
        .build_input_stream(
            config,
            move |data: &[f32], _| {
                let mono = pcm_f32_to_mono_i16_samples(data, channels);
                if let Some(meter) = orb_meter.as_ref() {
                    store_orb_input_level(meter, &mono);
                }
                let bytes = pcm_i16_samples_to_le_bytes(&resampler.push(&mono));
                if !bytes.is_empty() {
                    let sample_count = (bytes.len() / 2) as u64;
                    if audio_tx.try_send(bytes).is_ok() {
                        samples_sent.fetch_add(sample_count, Ordering::Relaxed);
                    }
                }
            },
            move |error| {
                let _ =
                    runtime_error_tx.send(format!("Live PTT microphone stream failed: {error}"));
            },
            None,
        )
        .map_err(|error| format!("failed to build live PTT f32 input stream: {error}"))
}

fn build_live_input_stream_i16(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    orb_meter: Option<Arc<AtomicU32>>,
    audio_tx: tokio_mpsc::Sender<Vec<u8>>,
    samples_sent: Arc<AtomicU64>,
    runtime_error_tx: mpsc::Sender<String>,
) -> Result<cpal::Stream, String> {
    let channels = config.channels.max(1) as usize;
    let mut resampler = Pcm16Resampler::new(config.sample_rate.0, LIVE_PTT_WIRE_SAMPLE_RATE_HZ);
    device
        .build_input_stream(
            config,
            move |data: &[i16], _| {
                let mono = pcm_i16_to_mono_i16_samples(data, channels);
                if let Some(meter) = orb_meter.as_ref() {
                    store_orb_input_level(meter, &mono);
                }
                let bytes = pcm_i16_samples_to_le_bytes(&resampler.push(&mono));
                if !bytes.is_empty() {
                    let sample_count = (bytes.len() / 2) as u64;
                    if audio_tx.try_send(bytes).is_ok() {
                        samples_sent.fetch_add(sample_count, Ordering::Relaxed);
                    }
                }
            },
            move |error| {
                let _ =
                    runtime_error_tx.send(format!("Live PTT microphone stream failed: {error}"));
            },
            None,
        )
        .map_err(|error| format!("failed to build live PTT i16 input stream: {error}"))
}

fn build_live_input_stream_u16(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    orb_meter: Option<Arc<AtomicU32>>,
    audio_tx: tokio_mpsc::Sender<Vec<u8>>,
    samples_sent: Arc<AtomicU64>,
    runtime_error_tx: mpsc::Sender<String>,
) -> Result<cpal::Stream, String> {
    let channels = config.channels.max(1) as usize;
    let mut resampler = Pcm16Resampler::new(config.sample_rate.0, LIVE_PTT_WIRE_SAMPLE_RATE_HZ);
    device
        .build_input_stream(
            config,
            move |data: &[u16], _| {
                let mono = pcm_u16_to_mono_i16_samples(data, channels);
                if let Some(meter) = orb_meter.as_ref() {
                    store_orb_input_level(meter, &mono);
                }
                let bytes = pcm_i16_samples_to_le_bytes(&resampler.push(&mono));
                if !bytes.is_empty() {
                    let sample_count = (bytes.len() / 2) as u64;
                    if audio_tx.try_send(bytes).is_ok() {
                        samples_sent.fetch_add(sample_count, Ordering::Relaxed);
                    }
                }
            },
            move |error| {
                let _ =
                    runtime_error_tx.send(format!("Live PTT microphone stream failed: {error}"));
            },
            None,
        )
        .map_err(|error| format!("failed to build live PTT u16 input stream: {error}"))
}

fn build_live_output_stream_f32(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: SharedPlaybackQueue,
    channels: usize,
) -> Result<cpal::Stream, String> {
    device
        .build_output_stream(
            config,
            move |data: &mut [f32], _| fill_output_f32(data, channels, &queue),
            log_stream_error,
            None,
        )
        .map_err(|error| format!("failed to build live PTT f32 output stream: {error}"))
}

fn build_live_output_stream_i16(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: SharedPlaybackQueue,
    channels: usize,
) -> Result<cpal::Stream, String> {
    device
        .build_output_stream(
            config,
            move |data: &mut [i16], _| fill_output_i16(data, channels, &queue),
            log_stream_error,
            None,
        )
        .map_err(|error| format!("failed to build live PTT i16 output stream: {error}"))
}

fn build_live_output_stream_u16(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: SharedPlaybackQueue,
    channels: usize,
) -> Result<cpal::Stream, String> {
    device
        .build_output_stream(
            config,
            move |data: &mut [u16], _| fill_output_u16(data, channels, &queue),
            log_stream_error,
            None,
        )
        .map_err(|error| format!("failed to build live PTT u16 output stream: {error}"))
}

fn write_f32_samples(data: &[f32], writer: &SharedWavWriter, samples_written: &AtomicU64) {
    let Ok(mut guard) = writer.lock() else {
        return;
    };
    let Some(writer) = guard.as_mut() else {
        return;
    };
    for sample in data {
        let sample = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        if writer.write_sample(sample).is_ok() {
            samples_written.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn write_i16_samples(data: &[i16], writer: &SharedWavWriter, samples_written: &AtomicU64) {
    let Ok(mut guard) = writer.lock() else {
        return;
    };
    let Some(writer) = guard.as_mut() else {
        return;
    };
    for sample in data {
        if writer.write_sample(*sample).is_ok() {
            samples_written.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn write_u16_samples(data: &[u16], writer: &SharedWavWriter, samples_written: &AtomicU64) {
    let Ok(mut guard) = writer.lock() else {
        return;
    };
    let Some(writer) = guard.as_mut() else {
        return;
    };
    for sample in data {
        let centered = (*sample as i32) - 32768;
        if writer.write_sample(centered as i16).is_ok() {
            samples_written.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(crate) fn pcm_f32_to_mono_i16_samples(data: &[f32], channels: usize) -> Vec<i16> {
    let mut samples = Vec::with_capacity(data.len() / channels);
    for frame in data.chunks(channels) {
        let sum: f32 = frame.iter().copied().sum();
        let sample =
            ((sum / frame.len().max(1) as f32).clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        samples.push(sample);
    }
    samples
}

pub(crate) fn pcm_i16_to_mono_i16_samples(data: &[i16], channels: usize) -> Vec<i16> {
    let mut samples = Vec::with_capacity(data.len() / channels);
    for frame in data.chunks(channels) {
        let sum: i32 = frame.iter().map(|sample| *sample as i32).sum();
        let sample =
            (sum / frame.len().max(1) as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        samples.push(sample);
    }
    samples
}

pub(crate) fn pcm_u16_to_mono_i16_samples(data: &[u16], channels: usize) -> Vec<i16> {
    let mut samples = Vec::with_capacity(data.len() / channels);
    for frame in data.chunks(channels) {
        let sum: i32 = frame.iter().map(|sample| *sample as i32 - 32768).sum();
        let sample =
            (sum / frame.len().max(1) as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        samples.push(sample);
    }
    samples
}

fn pcm_i16_samples_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

struct Pcm16Resampler {
    source_rate: u32,
    target_rate: u32,
    source_pos: f64,
    pending: Vec<i16>,
}

impl Pcm16Resampler {
    fn new(source_rate: u32, target_rate: u32) -> Self {
        Self {
            source_rate: source_rate.max(1),
            target_rate: target_rate.max(1),
            source_pos: 0.0,
            pending: Vec::new(),
        }
    }

    fn push(&mut self, samples: &[i16]) -> Vec<i16> {
        if samples.is_empty() {
            return Vec::new();
        }
        if self.source_rate == self.target_rate {
            return samples.to_vec();
        }
        self.pending.extend_from_slice(samples);
        let step = self.source_rate as f64 / self.target_rate as f64;
        let mut output = Vec::with_capacity(
            ((self.pending.len() as f64 - self.source_pos).max(0.0) / step).ceil() as usize,
        );
        while self.source_pos + 1.0 < self.pending.len() as f64 {
            let index = self.source_pos.floor() as usize;
            let frac = self.source_pos - index as f64;
            let a = self.pending[index] as f64;
            let b = self.pending[index + 1] as f64;
            output.push(
                (a + (b - a) * frac)
                    .round()
                    .clamp(i16::MIN as f64, i16::MAX as f64) as i16,
            );
            self.source_pos += step;
        }
        // The loop leaves `source_pos` up to one step past the last sample it
        // could interpolate, so the position can point beyond the buffer.
        // Drain only what exists and carry the remainder into the next push;
        // draining `floor(source_pos)` unclamped panicked on any buffer whose
        // length is not a multiple of the step (a whole 96 kHz voice note at
        // 6:1 — Live PTT chunks happened to divide evenly at 4:1).
        let consumed = (self.source_pos.floor() as usize).min(self.pending.len());
        if consumed > 0 {
            self.pending.drain(0..consumed);
            self.source_pos -= consumed as f64;
        }
        output
    }
}

fn fill_output_f32(data: &mut [f32], channels: usize, queue: &SharedPlaybackQueue) {
    let Ok(mut queue) = queue.lock() else {
        data.fill(0.0);
        return;
    };
    for frame in data.chunks_mut(channels.max(1)) {
        let sample = queue.pop_sample() as f32 / i16::MAX as f32;
        for output in frame {
            *output = sample;
        }
    }
}

fn fill_output_i16(data: &mut [i16], channels: usize, queue: &SharedPlaybackQueue) {
    let Ok(mut queue) = queue.lock() else {
        data.fill(0);
        return;
    };
    for frame in data.chunks_mut(channels.max(1)) {
        let sample = queue.pop_sample();
        for output in frame {
            *output = sample;
        }
    }
}

fn fill_output_u16(data: &mut [u16], channels: usize, queue: &SharedPlaybackQueue) {
    let Ok(mut queue) = queue.lock() else {
        data.fill(32768);
        return;
    };
    for frame in data.chunks_mut(channels.max(1)) {
        let sample = (queue.pop_sample() as i32 + 32768).clamp(0, u16::MAX as i32) as u16;
        for output in frame {
            *output = sample;
        }
    }
}

pub(crate) fn log_stream_error(error: cpal::StreamError) {
    warn!("Voice note microphone stream error: {}", error);
}

fn voice_note_temp_path() -> Result<PathBuf, String> {
    let mut dir = std::env::temp_dir();
    dir.push("magician-voice-notes");
    fs::create_dir_all(&dir).map_err(|error| {
        format!(
            "failed to create voice note temp dir {}: {error}",
            dir.display()
        )
    })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock error while naming voice note: {error}"))?
        .as_nanos();
    dir.push(format!("voice-note-{}-{nanos}.wav", std::process::id()));
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_free_input_gate_holds_through_playback_and_acoustic_tail() {
        let started = Instant::now();
        let mut gate = HalfDuplexInputGate::new(true);
        gate.note_output_started(started);

        assert!(!gate.allows_input(started, true, false));
        let drained = started + Duration::from_secs(2);
        assert!(!gate.allows_input(drained, false, false));
        assert!(!gate.allows_input(
            drained + LIVE_PTT_HANDS_FREE_ECHO_TAIL - Duration::from_millis(1),
            false,
            false,
        ));
        assert!(gate.allows_input(drained + LIVE_PTT_HANDS_FREE_ECHO_TAIL, false, false,));
    }

    #[test]
    fn realtime_and_muted_output_keep_input_open() {
        let now = Instant::now();
        let mut realtime = HalfDuplexInputGate::new(false);
        assert!(realtime.allows_input(now, true, false));

        let mut hands_free = HalfDuplexInputGate::new(true);
        hands_free.note_output_started(now);
        assert!(hands_free.allows_input(now, true, true));
    }

    #[test]
    fn streaming_quiet_requires_the_same_session_with_no_user_or_assistant_audio() {
        assert!(orb_streaming_is_semantically_quiet(Some(7), 7, false, None));
        assert!(!orb_streaming_is_semantically_quiet(
            Some(8),
            7,
            false,
            None
        ));
        assert!(!orb_streaming_is_semantically_quiet(Some(7), 7, true, None));
        assert!(!orb_streaming_is_semantically_quiet(
            Some(7),
            7,
            false,
            Some("response-1"),
        ));
    }

    #[test]
    fn playback_prebuffers_and_recovers_without_dropping_late_samples() {
        let mut queue = PlaybackQueueState::new(1_000);
        queue.begin_segment();
        queue.push_samples(std::iter::repeat(7).take(179));
        assert_eq!(queue.pop_sample(), 0);
        assert_eq!(queue.samples.len(), 179);

        queue.push_samples([8]);
        assert_eq!(queue.pop_sample(), 7);
        for _ in 0..179 {
            let _ = queue.pop_sample();
        }
        assert_eq!(queue.pop_sample(), 0);
        assert_eq!(queue.underrun_events, 1);

        queue.push_samples(std::iter::repeat(9).take(180));
        assert_eq!(queue.pop_sample(), 9);
    }

    #[test]
    fn playback_releases_short_tail_when_segment_finishes() {
        let mut queue = PlaybackQueueState::new(1_000);
        queue.begin_segment();
        queue.push_samples([11, 12, 13]);
        assert_eq!(queue.pop_sample(), 0);
        queue.end_segment();
        assert_eq!(queue.pop_sample(), 11);
        assert_eq!(queue.pop_sample(), 12);
        assert_eq!(queue.pop_sample(), 13);
    }

    #[test]
    fn next_segment_does_not_pause_a_buffered_previous_tail() {
        let mut queue = PlaybackQueueState::new(1_000);
        queue.begin_segment();
        queue.push_samples(std::iter::repeat(21).take(180));
        assert_eq!(queue.pop_sample(), 21);
        queue.end_segment();

        queue.begin_segment();
        assert_eq!(queue.pop_sample(), 21);
        assert_eq!(queue.samples.len(), 178);
    }

    #[test]
    fn semantic_tts_chunks_remain_one_gapless_playback_span() {
        let mut queue = PlaybackQueueState::new(1_000);
        queue.begin_segment();
        queue.push_samples(std::iter::repeat(31).take(400));
        for _ in 0..300 {
            assert_eq!(queue.pop_sample(), 31);
        }

        // The next semantic segment is appended while the same logical
        // response is active: no second prebuffer and no zero sample between
        // the two voices/emotions.
        queue.push_samples(std::iter::repeat(32).take(400));
        for _ in 0..100 {
            assert_eq!(queue.pop_sample(), 31);
        }
        assert_eq!(queue.pop_sample(), 32);
        assert_eq!(queue.underrun_events, 0);
    }

    #[test]
    fn mascot_tts_playback_guards_one_span_across_the_segment_loop() {
        let source = include_str!("voice_note.rs");
        let body = source
            .split_once("fn play_mascot_tts_segments(")
            .expect("mascot playback function")
            .1
            .split_once("fn emit_voice_note_state(")
            .expect("next function after mascot playback")
            .0;
        let loop_offset = body.find("for segment in segments").expect("segment loop");
        let begin_offset = body
            .find("playback.begin_segment()")
            .expect("logical playback begin");
        let end_offset = body
            .find("playback.end_segment()")
            .expect("logical playback end");
        let begin_guard = body
            .find("if !playback_started")
            .expect("single begin guard");
        let final_span_gate = body.rfind("if playback_started").expect("final span gate");

        assert!(begin_offset > loop_offset);
        assert!(begin_offset > begin_guard);
        assert_eq!(body.matches("playback.begin_segment()").count(), 1);
        assert_eq!(body.matches("playback.end_segment()").count(), 1);
        assert!(final_span_gate > begin_offset);
        assert!(end_offset > final_span_gate);
    }

    #[test]
    fn realtime_orb_fallback_is_single_mode_and_ownership_bounded() {
        assert!(should_fallback_orb_realtime(
            true,
            Some(OrbVoiceMode::Realtime),
            true
        ));
        assert!(!should_fallback_orb_realtime(
            true,
            Some(OrbVoiceMode::HandsFree),
            true
        ));
        assert!(!should_fallback_orb_realtime(
            true,
            Some(OrbVoiceMode::Realtime),
            false
        ));
    }

    #[test]
    fn realtime_orb_fallback_crosses_a_non_recursive_scheduler_boundary() {
        let source = include_str!("voice_note.rs");
        let scheduler = source
            .split_once("fn schedule_orb_hands_free_fallback(")
            .expect("fallback scheduler")
            .1
            .split_once("async fn engage_live_ptt(")
            .expect("engage follows scheduler")
            .0;
        assert!(scheduler.contains("tauri::async_runtime::spawn(async move"));
        assert!(scheduler.contains("engage_live_ptt(app.clone(), Some(OrbVoiceMode::HandsFree))"));

        let engage = source
            .split_once("async fn engage_live_ptt(")
            .expect("engage function")
            .1
            .split_once("async fn release_live_ptt(")
            .expect("release follows engage")
            .0;
        assert!(engage.contains("schedule_orb_hands_free_fallback(ws_app.clone())"));
        assert!(!engage.contains("Box::pin(engage_live_ptt("));
    }

    fn response(
        transcript: &str,
        assistant_preview: Option<&str>,
        queued_message_id: Option<&str>,
    ) -> VoiceNoteSubmitResponse {
        VoiceNoteSubmitResponse {
            chat_session_id: "chat-1".to_string(),
            chat_turn_id: None,
            transcript: transcript.to_string(),
            assistant_preview: assistant_preview.map(str::to_string),
            assistant_speech_segments: None,
            queued_message_id: queued_message_id.map(str::to_string),
        }
    }

    #[test]
    fn live_ptt_recoverable_errors_do_not_terminate_the_session() {
        assert!(live_ptt_error_is_recoverable(&json!({
            "message": "local captions unavailable",
            "recoverable": true,
        })));
        assert!(!live_ptt_error_is_recoverable(&json!({
            "message": "provider disconnected",
            "recoverable": false,
        })));
        assert!(!live_ptt_error_is_recoverable(&json!({
            "message": "legacy fatal error",
        })));
    }

    #[test]
    fn live_ptt_session_ready_resolves_the_backend_agent_name() {
        assert_eq!(
            assistant_name_from_session_ready(&json!({
                "agent": {"agent_id": "agent-sam", "name": "  Sam  ", "is_primary": true}
            }))
            .as_deref(),
            Some("Sam")
        );
        assert_eq!(
            assistant_name_from_session_ready(&json!({"agent": {"name": "   "}})),
            None
        );
        assert_eq!(assistant_name_from_session_ready(&json!({})), None);
    }

    #[test]
    fn live_ptt_ignored_transcript_feedback_is_reason_aware() {
        assert_eq!(
            ignored_transcript_feedback(&json!({"reason": "no_final_transcript"})),
            IgnoredTranscriptFeedback::Silent
        );
        assert_eq!(
            ignored_transcript_feedback(&json!({"reason": "self_echo"})),
            IgnoredTranscriptFeedback::Silent
        );
        assert_eq!(
            ignored_transcript_feedback(&json!({"reason": "address_prefix_armed"})),
            IgnoredTranscriptFeedback::Guidance("Listening — go ahead.".to_string())
        );
        assert_eq!(
            ignored_transcript_feedback(&json!({
                "reason": "address_prefix_required",
                "activation_phrases": ["Hey Presto", "Hey Sam"]
            })),
            IgnoredTranscriptFeedback::Guidance("Start with “Hey Presto”.".to_string())
        );
        assert_eq!(
            ignored_transcript_feedback(&json!({"reason": "future_reason"})),
            IgnoredTranscriptFeedback::Failure("Voice input was not sent.".to_string())
        );
    }

    #[test]
    fn voice_success_message_summarizes_preview() {
        let message = voice_note_success_message(&response(
            "show my latest tasks",
            Some("Here are the latest tasks."),
            None,
        ));
        assert!(message.contains("You: show my latest tasks"));
        assert!(message.contains("Assistant: Here are the latest tasks."));
    }

    #[test]
    fn voice_success_message_marks_queued_work() {
        let message =
            voice_note_success_message(&response("summarize this thread", None, Some("q1")));
        assert!(message.contains("Assistant is working on it."));
    }

    #[test]
    fn voice_visual_state_prefers_thinking_for_queued_work_without_preview() {
        assert_eq!(
            voice_visual_state_for_response(&response("do work", None, Some("q1"))),
            "thinking"
        );
        assert_eq!(
            voice_visual_state_for_response(&response("answer now", Some("Done."), None)),
            "speaking"
        );
    }

    #[test]
    fn voice_tts_segments_prefer_backend_segments_over_preview() {
        let mut response = response("status", Some("<speech>preview only</speech>"), None);
        response.assistant_speech_segments = Some(vec![VoiceSpeechSegment {
            text: "full backend segment".to_string(),
            emotion: Some("happy".to_string()),
            style: None,
            pace: None,
            voice_mode: None,
            emphasis: None,
        }]);
        let segments = voice_response_segments_for_tts(&response);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "full backend segment");
        assert_eq!(segments[0].emotion.as_deref(), Some("happy"));
    }

    #[test]
    fn voice_tts_fallback_strips_speech_tags_before_speaking() {
        let segments = voice_response_segments_for_tts(&response(
            "status",
            Some(r#"<speech emotion="happy">Done now.</speech>"#),
            None,
        ));
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "Done now.");
    }

    #[test]
    fn merge_voice_note_payload_adds_source_and_thread() {
        let payload = merge_voice_note_payload(json!({ "sequence": 7 }), "general");
        assert_eq!(payload["sequence"], 7);
        assert_eq!(payload["source_surface"], VOICE_NOTE_SOURCE_SURFACE);
        assert_eq!(payload["thread_id"], "general");
        assert!(payload["presence_session_id"]
            .as_str()
            .unwrap_or_default()
            .starts_with("desktop-tray-"));
    }

    #[test]
    fn merge_live_ptt_payload_adds_source_and_thread() {
        let payload = merge_live_ptt_payload(json!({ "command": "ptt.engage" }), "voice");
        assert_eq!(payload["command"], "ptt.engage");
        assert_eq!(payload["source_surface"], LIVE_PTT_SOURCE_SURFACE);
        assert_eq!(payload["thread_id"], "voice");
        assert!(payload["presence_session_id"]
            .as_str()
            .unwrap_or_default()
            .starts_with("desktop-tray-"));
    }

    #[test]
    fn live_ptt_control_command_labels_are_stable_wire_contract() {
        assert_eq!(LivePttControlCommand::Engage.label(), "ptt.engage");
        assert_eq!(LivePttControlCommand::Release.label(), "ptt.release");
        assert_eq!(LivePttControlCommand::ClearInput.label(), "input.clear");
        assert_eq!(
            LivePttControlCommand::Interrupt.label(),
            "response.interrupt"
        );
        assert_eq!(
            LivePttControlCommand::SetOutputMuted(true).label(),
            "output.mute"
        );
        assert_eq!(
            LivePttControlCommand::SetOutputMuted(false).label(),
            "output.unmute"
        );
        assert_eq!(LivePttControlCommand::End.label(), "session.end");
    }

    #[test]
    fn first_live_engage_does_not_emit_an_unconditional_response_interrupt() {
        let source = include_str!("voice_note.rs");
        let engage = source
            .split_once("LivePttControlCommand::Engage => {")
            .expect("engage command branch")
            .1
            .split_once("LivePttControlCommand::Release => {")
            .expect("release follows engage")
            .0;

        assert!(engage.contains("send_ws_json(&mut ws_write, \"ptt.engage\""));
        assert!(!engage.contains("send_ws_json(&mut ws_write, \"response.interrupt\""));
    }

    #[test]
    fn hands_free_preference_uses_the_live_native_transport() {
        assert_eq!(ptt_mode_from_voice_mode("hands_free"), PTT_MODE_LIVE);
        assert_eq!(ptt_mode_from_voice_mode("handsfree"), PTT_MODE_LIVE);
        assert_eq!(ptt_mode_from_voice_mode("recording"), PTT_MODE_DICTATE);
    }

    #[test]
    fn ambient_realtime_serializes_an_automatic_turn_boundary() {
        assert_eq!(
            live_ptt_turn_boundary(Some(OrbVoiceMode::Realtime), false),
            Some("server_vad")
        );
        assert_eq!(
            live_ptt_turn_boundary(Some(OrbVoiceMode::Realtime), true),
            None
        );
        assert_eq!(
            live_ptt_turn_boundary(Some(OrbVoiceMode::HandsFree), false),
            None
        );
        assert_eq!(live_ptt_turn_boundary(None, false), None);

        let ambient = live_ptt_session_start_payload(
            "general",
            "voice_realtime_openai_backend",
            "realtime",
            live_ptt_turn_boundary(Some(OrbVoiceMode::Realtime), false),
        );
        assert_eq!(ambient["turn_boundary"], "server_vad");

        let push_to_talk = live_ptt_session_start_payload(
            "general",
            "voice_realtime_openai_backend",
            "realtime",
            live_ptt_turn_boundary(Some(OrbVoiceMode::Realtime), true),
        );
        assert!(push_to_talk.get("turn_boundary").is_none());
    }

    #[test]
    fn native_live_ptt_payload_cannot_supply_app_or_provider_authority() {
        let payload = live_ptt_session_start_payload(
            "general",
            "voice_realtime_openai_backend",
            "realtime",
            Some("server_vad"),
        );
        let encoded = serde_json::to_string(&payload).unwrap();
        for forbidden in [
            "credential",
            "authority",
            "provider_trust",
            "processing_class",
            "storage_policy",
            "actor_ref",
            "session_ref",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "native session.start must not author `{forbidden}`: {encoded}"
            );
        }

        let registration_source = include_str!("voice_note.rs")
            .split_once("async fn register_live_ptt_media_session")
            .expect("registration function")
            .1
            .split_once("async fn post_live_ptt_session_event")
            .expect("registration function boundary")
            .0;
        for forbidden in ["credential", "authority", "provider_trust"] {
            assert!(!registration_source.contains(forbidden));
        }
    }

    #[test]
    fn ambient_orb_mode_is_independent_and_routes_all_three_engines() {
        assert_eq!(
            configured_orb_voice_mode("realtime"),
            OrbVoiceMode::Realtime
        );
        assert_eq!(configured_orb_voice_mode(" LIVE "), OrbVoiceMode::Realtime);
        assert_eq!(
            configured_orb_voice_mode("hands_free"),
            OrbVoiceMode::HandsFree
        );
        assert_eq!(
            configured_orb_voice_mode("recording"),
            OrbVoiceMode::Dictation
        );
        assert_eq!(
            configured_orb_voice_mode("dictate"),
            OrbVoiceMode::Dictation
        );
        assert_eq!(configured_orb_voice_mode(""), OrbVoiceMode::HandsFree);
        assert_eq!(OrbVoiceMode::Dictation.streaming_wire_value(), None);
        assert_eq!(
            OrbVoiceMode::HandsFree.streaming_wire_value(),
            Some("hands_free")
        );
        assert_eq!(
            OrbVoiceMode::Realtime.streaming_wire_value(),
            Some("realtime")
        );
        // The admitted mode, not a later Settings read, owns an in-flight
        // startup. Here the ordinary configured value has already changed to
        // Dictation, but the admitted realtime route remains realtime.
        assert_eq!(
            live_ptt_wire_voice_mode("recording", Some(OrbVoiceMode::Realtime)).as_deref(),
            Ok("realtime")
        );
        assert_eq!(
            live_ptt_wire_voice_mode("realtime", Some(OrbVoiceMode::HandsFree)).as_deref(),
            Ok("hands_free")
        );
        assert!(live_ptt_wire_voice_mode("realtime", Some(OrbVoiceMode::Dictation)).is_err());
        assert_eq!(
            live_ptt_wire_voice_mode("recording", None).as_deref(),
            Ok("recording")
        );
    }

    #[test]
    fn ambient_dictation_gate_ends_on_silence_no_speech_and_hard_cap() {
        let mut trailing = AmbientDictationSilenceGate::new(Duration::from_secs(8));
        assert_eq!(trailing.observe(0.02, Duration::from_secs(1)), None);
        assert_eq!(trailing.observe(0.0, Duration::from_millis(2_099)), None);
        assert_eq!(
            trailing.observe(0.0, Duration::from_millis(2_100)),
            Some(AmbientDictationBoundary::SpeechComplete)
        );

        let mut quiet = AmbientDictationSilenceGate::new(Duration::from_secs(8));
        assert_eq!(quiet.observe(0.0, Duration::from_millis(7_999)), None);
        assert_eq!(
            quiet.observe(0.0, Duration::from_secs(8)),
            Some(AmbientDictationBoundary::NoSpeech)
        );

        let mut capped = AmbientDictationSilenceGate::new(Duration::from_secs(8));
        assert_eq!(capped.observe(0.02, Duration::from_secs(1)), None);
        assert_eq!(
            capped.observe(0.02, AMBIENT_DICTATION_MAX_UTTERANCE),
            Some(AmbientDictationBoundary::SpeechComplete)
        );
    }

    /// Drive the gate at its real poll cadence with one level per tick.
    fn drive_gate(
        gate: &mut AmbientDictationSilenceGate,
        levels: impl IntoIterator<Item = f32>,
        start_tick: u64,
    ) -> (u64, Option<AmbientDictationBoundary>) {
        let mut tick = start_tick;
        for level in levels {
            tick += 1;
            let elapsed = AMBIENT_DICTATION_POLL_INTERVAL * tick as u32;
            if let Some(boundary) = gate.observe(level, elapsed) {
                return (tick, Some(boundary));
            }
        }
        (tick, None)
    }

    /// The measured failure: room noise above the fixed line. Before the
    /// adaptive floor a four-second question waited 35 s for a quiet dip and
    /// an empty room ran the 45 s cap as "speech" with an empty transcript.
    #[test]
    fn ambient_dictation_gate_adapts_to_room_noise() {
        let noise = 0.03_f32; // above AMBIENT_DICTATION_SPEECH_LEVEL

        // Noise only: the floor settles on it and the turn ends as no speech
        // at the follow-up window, not as 45 s of speech.
        let mut empty_room = AmbientDictationSilenceGate::new(Duration::from_secs(8));
        let (tick, boundary) = drive_gate(&mut empty_room, std::iter::repeat(noise).take(100), 0);
        assert_eq!(boundary, Some(AmbientDictationBoundary::NoSpeech));
        assert_eq!(tick, 80, "no speech at exactly follow_up_seconds");

        // Noise, a two-second question, noise: the turn closes 1.1 s after the
        // question, not at the cap.
        let mut question = AmbientDictationSilenceGate::new(Duration::from_secs(8));
        let levels = std::iter::repeat(noise)
            .take(10)
            .chain(std::iter::repeat(0.2).take(20))
            .chain(std::iter::repeat(noise).take(30));
        let (tick, boundary) = drive_gate(&mut question, levels, 0);
        assert_eq!(boundary, Some(AmbientDictationBoundary::SpeechComplete));
        assert_eq!(tick, 30 + 11, "closes after the trailing-silence window");

        // A short pause inside a sentence does not end the turn.
        let mut pause = AmbientDictationSilenceGate::new(Duration::from_secs(8));
        let levels = std::iter::repeat(noise)
            .take(5)
            .chain(std::iter::repeat(0.2).take(10))
            .chain(std::iter::repeat(noise).take(6))
            .chain(std::iter::repeat(0.2).take(10));
        let (_, boundary) = drive_gate(&mut pause, levels, 0);
        assert_eq!(boundary, None);

        // Speech never raises the floor: a long monologue that starts the
        // instant the wake phrase ends keeps counting for its whole length.
        let mut monologue = AmbientDictationSilenceGate::new(Duration::from_secs(8));
        let (_, boundary) = drive_gate(&mut monologue, std::iter::repeat(0.2).take(300), 0);
        assert_eq!(boundary, None);
        assert!(monologue.speech_threshold() < 0.2);

        // Eager talker in a noisy room: speech from the first tick leaves no
        // calibration window, yet the lull after it still finds the floor and
        // the turn closes within a few seconds instead of at the 45 s cap.
        let mut eager = AmbientDictationSilenceGate::new(Duration::from_secs(8));
        let levels = std::iter::repeat(0.2)
            .take(20)
            .chain(std::iter::repeat(noise).take(200));
        let (tick, boundary) = drive_gate(&mut eager, levels, 0);
        assert_eq!(boundary, Some(AmbientDictationBoundary::SpeechComplete));
        assert!(
            (31..=50).contains(&tick),
            "closed at tick {tick}, expected within ~3 s of the last word"
        );

        // A quiet room keeps the old fixed line.
        let quiet = AmbientDictationSilenceGate::new(Duration::from_secs(8));
        assert_eq!(quiet.speech_threshold(), AMBIENT_DICTATION_SPEECH_LEVEL);
    }

    /// The microphone records at its native rate (96 kHz here); the note is
    /// uploaded twice on its way to transcription, so it travels as 16 kHz
    /// mono. The waveform survives: a 440 Hz tone keeps its period, a stereo
    /// pair is averaged, and an already-small note is left alone.
    #[test]
    fn voice_notes_are_downsampled_to_16k_mono_for_transcription() {
        fn wav(rate: u32, channels: u16, seconds: f32, f32_format: bool) -> Vec<u8> {
            let mut cursor = Cursor::new(Vec::new());
            let spec = WavSpec {
                channels,
                sample_rate: rate,
                bits_per_sample: if f32_format { 32 } else { 16 },
                sample_format: if f32_format {
                    SampleFormat::Float
                } else {
                    SampleFormat::Int
                },
            };
            let mut writer = WavWriter::new(&mut cursor, spec).unwrap();
            let frames = (rate as f32 * seconds) as usize;
            for frame in 0..frames {
                let t = frame as f32 / rate as f32;
                let value = (t * 440.0 * std::f32::consts::TAU).sin() * 0.5;
                for _ in 0..channels {
                    if f32_format {
                        writer.write_sample(value).unwrap();
                    } else {
                        writer
                            .write_sample((value * i16::MAX as f32) as i16)
                            .unwrap();
                    }
                }
            }
            writer.finalize().unwrap();
            cursor.into_inner()
        }

        let native = wav(96_000, 1, 0.5, false);
        let small = downsample_wav_for_transcription(&native).unwrap();
        assert!(
            small.len() * 5 < native.len(),
            "{} -> {} bytes",
            native.len(),
            small.len()
        );
        let reader = WavReader::new(Cursor::new(&small)).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.spec().channels, 1);
        let samples = reader
            .into_samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            (samples.len() as i64 - 8_000).abs() <= 8,
            "{} samples",
            samples.len()
        );
        // 440 Hz at 16 kHz: a zero crossing every ~18 samples → ~440 rising
        // crossings per second, i.e. ~220 in half a second.
        let rising = samples
            .windows(2)
            .filter(|pair| pair[0] < 0 && pair[1] >= 0)
            .count();
        assert!((200..=240).contains(&rising), "{rising} rising crossings");

        let stereo_f32 = wav(48_000, 2, 0.25, true);
        let mono = downsample_wav_for_transcription(&stereo_f32).unwrap();
        let reader = WavReader::new(Cursor::new(&mono)).unwrap();
        assert_eq!(
            (reader.spec().sample_rate, reader.spec().channels),
            (16_000, 1)
        );

        let already_small = wav(16_000, 1, 0.1, false);
        assert_eq!(
            downsample_wav_for_transcription(&already_small).unwrap(),
            already_small
        );
    }

    /// A buffer whose length is not a multiple of the step used to leave
    /// `source_pos` past the end and panic the recorder thread on drain — the
    /// shape of every whole voice note at 96 kHz → 16 kHz.
    #[test]
    fn resampler_survives_buffers_that_do_not_divide_by_the_step() {
        for len in [1usize, 5, 7, 10, 11, 2_785_280 % 1_000 + 1_000, 46_421] {
            let samples = (0..len).map(|i| (i % 200) as i16 * 100).collect::<Vec<_>>();
            let mut resampler = Pcm16Resampler::new(96_000, 16_000);
            let out = resampler.push(&samples);
            assert!(out.len() <= len / 6 + 1, "len {len}: {} out", out.len());
            // Pushing again continues from the carried position without loss.
            let more = resampler.push(&samples);
            assert!(more.len() <= len / 6 + 2, "len {len}: {} more", more.len());
        }
        // Streaming in odd chunks yields the same count as one push.
        let signal = (0..1_003).map(|i| (i % 50) as i16).collect::<Vec<_>>();
        let mut whole = Pcm16Resampler::new(96_000, 16_000);
        let whole_len = whole.push(&signal).len();
        let mut chunked = Pcm16Resampler::new(96_000, 16_000);
        let chunked_len: usize = signal
            .chunks(7)
            .map(|chunk| chunked.push(chunk).len())
            .sum();
        assert_eq!(whole_len, chunked_len);
    }

    #[test]
    fn ambient_dictation_input_meter_normalizes_all_native_sample_formats() {
        assert_eq!(input_f32_rms(&[]), 0.0);
        assert_eq!(input_f32_rms(&[0.0; 16]), 0.0);
        assert!(input_f32_rms(&[0.25; 16]) > 0.0);
        assert_eq!(pcm16_rms(&[0; 16]), 0.0);
        assert!(pcm16_rms(&[8_000; 16]) > 0.0);
        assert_eq!(input_u16_rms(&[32_768; 16]), 0.0);
        assert!(input_u16_rms(&[40_000; 16]) > 0.0);
        assert!(input_f32_rms(&[1.0; 16]) <= 1.0);
        assert!(input_u16_rms(&[u16::MAX; 16]) <= 1.0);
    }

    #[test]
    fn ambient_dictation_resolves_primary_agent_name_from_backend_payload() {
        let payload = json!({
            "agents": [
                {"definition": {"name": "Worker", "is_primary": false}},
                {"definition": {"name": "  Sam  ", "is_primary": true}}
            ]
        });
        assert_eq!(
            primary_agent_name_from_payload(&payload).as_deref(),
            Some("Sam")
        );
        assert_eq!(
            primary_agent_name_from_payload(&json!({
                "agents": [{"definition": {"name": "", "is_primary": true}}]
            })),
            None
        );
    }

    #[test]
    fn ambient_dictation_treats_typed_language_rejection_as_recoverable_guidance() {
        let rejection = VoiceNoteBackendRejection::from_response(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "error": "unsupported_language",
                "message": "Please try again in English.",
                "chat_session_id": "chat-voice-1",
                "audio_artifact_id": "att-1"
            })
            .to_string(),
        );

        assert!(rejection.is_recoverable_ambient_dictation());
        assert_eq!(rejection.spoken_guidance(), "Please try again in English.");
        assert_eq!(rejection.chat_session_id.as_deref(), Some("chat-voice-1"));
        assert_eq!(
            rejection.to_string(),
            "The backend returned 422 Unprocessable Entity: Please try again in English."
        );
    }

    #[test]
    fn ambient_dictation_keeps_unknown_backend_failures_terminal() {
        let rejection = VoiceNoteBackendRejection::from_response(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            json!({
                "error": "stt_unavailable",
                "message": "Speech recognition is unavailable."
            })
            .to_string(),
        );

        assert!(!rejection.is_recoverable_ambient_dictation());
        assert_eq!(rejection.chat_session_id, None);
    }

    #[test]
    fn orb_session_owner_rejects_stale_cleanup_and_ordinary_teardown() {
        let owner = AtomicU64::new(ORB_SESSION_PENDING);
        assert!(claim_orb_session(&owner, 41));
        assert!(!release_orb_session(&owner, 40));
        assert_eq!(owner.load(Ordering::Acquire), 41);
        assert!(release_orb_session(&owner, 41));
        assert_eq!(owner.load(Ordering::Acquire), ORB_SESSION_NONE);
        assert!(!release_orb_session(&owner, 99));

        owner.store(ORB_SESSION_PENDING, Ordering::Release);
        assert!(claim_orb_session(&owner, 42));
        assert!(!release_orb_session(&owner, 41));
        assert_eq!(owner.load(Ordering::Acquire), 42);
    }

    #[test]
    fn orb_sequence_never_uses_owner_sentinels_when_the_counter_wraps() {
        assert_eq!(next_orb_sequence(0), 1);
        assert_eq!(next_orb_sequence(41), 42);
        assert_eq!(next_orb_sequence(u64::MAX - 1), 1);
        assert_eq!(next_orb_sequence(u64::MAX), 1);
    }

    #[test]
    fn external_voice_lease_rejects_stale_cleanup() {
        let owner = AtomicU64::new(17);
        assert!(!release_external_voice_owner(&owner, 16));
        assert_eq!(owner.load(Ordering::Acquire), 17);
        assert!(release_external_voice_owner(&owner, 17));
        assert_eq!(owner.load(Ordering::Acquire), ORB_SESSION_NONE);

        owner.store(18, Ordering::Release);
        assert!(!release_external_voice_owner(&owner, 17));
        assert_eq!(owner.load(Ordering::Acquire), 18);
    }

    #[test]
    fn assistant_delta_is_the_streaming_caption_protocol() {
        assert_eq!(
            assistant_caption_final("transcript.assistant.delta"),
            Some(false)
        );
        assert_eq!(assistant_caption_final("transcript.assistant"), Some(true));
        assert_eq!(
            assistant_caption_final("transcript.assistant.partial"),
            None
        );
    }

    #[test]
    fn interrupted_control_frames_clear_native_playback() {
        assert!(live_ptt_control_clears_playback(
            r#"{"kind":"response.interrupted","payload":{"response_id":"r1"}}"#
        ));
        assert!(live_ptt_control_clears_playback(
            r#"{"kind":"audio.output.ended","payload":{"interrupted":true}}"#
        ));
        assert!(!live_ptt_control_clears_playback(
            r#"{"kind":"audio.output.ended","payload":{"interrupted":false}}"#
        ));
    }

    #[test]
    fn stale_output_end_cannot_finish_a_newer_response() {
        assert!(!output_end_matches(
            Some("response-new"),
            Some("response-old")
        ));
        assert!(output_end_matches(
            Some("response-new"),
            Some("response-new")
        ));
        assert!(output_end_matches(Some("response-new"), None));
        assert!(output_end_matches(None, Some("response-old")));
        assert_eq!(
            live_response_id(&json!({"response": {"id": "nested"}})).as_deref(),
            Some("nested")
        );
    }

    #[test]
    fn orb_audio_meter_has_a_noise_floor_and_reaches_full_scale() {
        assert_eq!(pcm16_rms(&[]), 0.0);
        assert_eq!(pcm16_rms(&[20, -20, 15, -15]), 0.0);
        assert!(pcm16_rms(&[3_000, -3_000, 3_000, -3_000]) > 0.4);
        assert_eq!(pcm16_rms(&[i16::MAX, i16::MIN]), 1.0);
    }

    #[test]
    fn orb_output_meter_decodes_little_endian_pcm16() {
        let samples = [4_000_i16, -4_000_i16];
        let bytes = samples
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();
        assert!((pcm16_le_bytes_rms(&bytes) - pcm16_rms(&samples)).abs() < f32::EPSILON);
        assert_eq!(pcm16_le_bytes_rms(&[1]), 0.0);
    }

    #[test]
    fn voice_recording_test_wav_conversion_prepares_playback_audio() {
        let spec = WavSpec {
            channels: 2,
            sample_rate: LIVE_PTT_WIRE_SAMPLE_RATE_HZ,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).unwrap();
            for index in 0..480 {
                let sample = if index % 2 == 0 { 1200_i16 } else { -1200_i16 };
                writer.write_sample(sample).unwrap();
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        let bytes = cursor.into_inner();
        let (playback, duration) = wav_bytes_to_live_ptt_pcm16(&bytes).unwrap();
        assert!(!playback.is_empty());
        assert_eq!(playback.len(), 480 * 2);
        assert!(duration.as_millis() > 0);
    }

    #[test]
    fn macos_tts_float32_wav_conversion_prepares_pcm16_playback_audio() {
        let spec = WavSpec {
            channels: 2,
            sample_rate: 22_050,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).unwrap();
            for index in 0..441 {
                let sample = if index % 2 == 0 { 0.25_f32 } else { -0.25_f32 };
                writer.write_sample(sample).unwrap();
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();
        }

        let (playback, duration) =
            wav_bytes_to_live_ptt_pcm16_from_source(&cursor.into_inner(), "macOS TTS").unwrap();
        assert!(!playback.is_empty());
        assert_eq!(playback.len() % std::mem::size_of::<i16>(), 0);
        assert_eq!(duration.as_millis(), 20);
    }

    #[test]
    fn macos_tts_float32_wav_discards_an_incomplete_final_sample() {
        let spec = WavSpec {
            channels: 1,
            sample_rate: 22_050,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).unwrap();
            writer.write_sample(0.25_f32).unwrap();
            writer.write_sample(-0.25_f32).unwrap();
            writer.finalize().unwrap();
        }
        let mut bytes = cursor.into_inner();
        let data_marker = bytes
            .windows(4)
            .position(|window| window == b"data")
            .expect("Float32 WAV data chunk");
        let data_len_offset = data_marker + 4;
        let declared_data_len = u32::from_le_bytes(
            bytes[data_len_offset..data_len_offset + 4]
                .try_into()
                .unwrap(),
        );
        bytes.push(0x7f);
        bytes[data_len_offset..data_len_offset + 4]
            .copy_from_slice(&(declared_data_len + 1).to_le_bytes());
        let declared_riff_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        bytes[4..8].copy_from_slice(&(declared_riff_len + 1).to_le_bytes());

        assert!(WavReader::new(Cursor::new(bytes.as_slice())).is_err());
        let (repaired, discarded, normalized_streaming_length) =
            align_supported_wav_data_chunk(&bytes);
        assert_eq!(discarded, 1);
        assert!(!normalized_streaming_length);
        assert!(WavReader::new(Cursor::new(repaired.as_ref())).is_ok());
        assert!(wav_bytes_to_live_ptt_pcm16_from_source(&bytes, "macOS TTS").is_ok());
    }

    #[test]
    fn tts_wav_conversion_discards_only_an_incomplete_final_pcm16_sample() {
        let mut bytes = pcm16_test_wav(1, &[1_200, -1_200]);
        let declared_data_len = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
        bytes.extend_from_slice(&[0x7f]);
        bytes[40..44].copy_from_slice(&(declared_data_len + 1).to_le_bytes());
        let declared_riff_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        bytes[4..8].copy_from_slice(&(declared_riff_len + 1).to_le_bytes());

        assert!(WavReader::new(Cursor::new(bytes.as_slice())).is_err());
        let (repaired, discarded, normalized_streaming_length) =
            align_supported_wav_data_chunk(&bytes);
        assert_eq!(discarded, 1);
        assert!(!normalized_streaming_length);
        assert!(matches!(repaired, Cow::Owned(_)));

        let (playback, duration) =
            wav_bytes_to_live_ptt_pcm16_from_source(&bytes, "test TTS").unwrap();
        assert_eq!(playback.len(), 2 * std::mem::size_of::<i16>());
        assert!(duration.as_nanos() > 0);
    }

    #[test]
    fn openai_streaming_wav_length_sentinel_is_canonicalized_after_collection() {
        let mut bytes = pcm16_test_wav(1, &[1_200, -1_200]);
        bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());

        assert!(WavReader::new(Cursor::new(bytes.as_slice())).is_err());
        let (repaired, discarded, normalized_streaming_length) =
            align_supported_wav_data_chunk(&bytes);
        assert_eq!(discarded, 0);
        assert!(normalized_streaming_length);
        assert_eq!(
            u32::from_le_bytes(repaired[4..8].try_into().unwrap()),
            u32::try_from(repaired.len() - 8).unwrap()
        );
        assert_eq!(u32::from_le_bytes(repaired[40..44].try_into().unwrap()), 4);
        assert!(WavReader::new(Cursor::new(repaired.as_ref())).is_ok());
        assert!(wav_bytes_to_live_ptt_pcm16_from_source(&bytes, "OpenAI TTS").is_ok());
    }

    #[test]
    fn tts_wav_conversion_aligns_stereo_to_a_complete_frame() {
        let mut bytes = pcm16_test_wav(2, &[1_200, 1_200]);
        let declared_data_len = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
        bytes.extend_from_slice(&1_200_i16.to_le_bytes());
        bytes[40..44].copy_from_slice(&(declared_data_len + 2).to_le_bytes());
        let declared_riff_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        bytes[4..8].copy_from_slice(&(declared_riff_len + 2).to_le_bytes());

        let (repaired, discarded, normalized_streaming_length) =
            align_supported_wav_data_chunk(&bytes);
        assert_eq!(discarded, 2);
        assert!(!normalized_streaming_length);
        let repaired_reader = WavReader::new(Cursor::new(repaired.as_ref())).unwrap();
        assert_eq!(repaired_reader.len(), 2);
    }

    #[test]
    fn tts_wav_conversion_does_not_mask_a_truncated_data_chunk() {
        let mut bytes = pcm16_test_wav(1, &[1_200, -1_200]);
        bytes[40..44].copy_from_slice(&200_u32.to_le_bytes());

        let (candidate, discarded, normalized_streaming_length) =
            align_supported_wav_data_chunk(&bytes);
        assert_eq!(discarded, 0);
        assert!(!normalized_streaming_length);
        assert!(matches!(candidate, Cow::Borrowed(_)));
        assert!(wav_bytes_to_live_ptt_pcm16_from_source(&bytes, "test TTS").is_err());
    }

    fn pcm16_test_wav(channels: u16, samples: &[i16]) -> Vec<u8> {
        let spec = WavSpec {
            channels,
            sample_rate: LIVE_PTT_WIRE_SAMPLE_RATE_HZ,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).unwrap();
            for sample in samples {
                writer.write_sample(*sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        let bytes = cursor.into_inner();
        assert_eq!(&bytes[36..40], b"data");
        bytes
    }
}
