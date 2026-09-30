//! Continuous screen observation — the P7 rail of
//! `docs/archive/plans/2026-06-11-screen-capture-and-ask.md`.
//!
//! A general-purpose "watch my screen" session with NO meeting coupling: it
//! borrows the passive meeting listener's lifecycle architecture (manager,
//! cancel token, pause gate, idle auto-stop, teardown summary + bounded
//! memory write, transcript-sink streaming) because those problems are
//! identical — frames here play the role audio plays there.
//!
//! The loop: a still every `cadence_s` via `screencapture -x` → a dHash
//! frame-diff gate (ffmpeg downscale to 9×8 gray; an UNCHANGED screen costs
//! zero model calls and retains zero data — the gate is both the cost clamp
//! and a privacy property) → changed frames go to the [`FrameNarrator`] with
//! the session's purpose → `nothing` (dropped) / `note` (narrated into the
//! session) / `alert` (narrated + flagged; `watch` mode may stop itself).
//!
//! Narration is the durable record: frames are TRANSIENT (hashed, analyzed,
//! deleted). Teardown posts a final summary into the session (speaker
//! `Observation summary` — the same durable-record rule the meeting rails
//! learned) and appends ONE bounded entry to the `screen_observations` tier.
//!
//! The narrator routes through the process-global `OperationLlmRouter`:
//! the `screen_observation` operation in `magician-config.yaml` maps it to a
//! vision profile (gpt-5.6-terra) — the model lives in CONFIG like every
//! other LLM call, never in env vars. The teardown summarizer routes the
//! same way (`meeting_summary` operation via `default_summarizer()`).
//! Behavioral env knobs (cadence/idle/diff/coalescing) remain below.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::json;
use tokio::{process::Command, sync::Mutex};
use tokio_util::sync::CancellationToken;

use super::meeting::{ChatThreadTranscriptSink, Summarizer, TranscriptSink, TranscriptTurn};
use super::providers::{host_gateway_url_from_env, HostAutomationProvider, ScreenCaptureRequest};
use super::AudioStage;
use magician::magician_v2::execution::agent_resources::AgentResources;

// ─── env knobs ──────────────────────────────────────────────────────────

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

/// Seconds between capture ticks (`SCREEN_OBSERVE_CADENCE_SECS`).
pub fn default_cadence_s() -> u64 {
    env_u64("SCREEN_OBSERVE_CADENCE_SECS", 4)
}

/// Stop after this long with NO screen change (the diff gate computes
/// "unchanged" for free). 0 disables.
fn idle_stop() -> Option<Duration> {
    let secs = env_u64("SCREEN_OBSERVE_IDLE_STOP_SECS", 15 * 60);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// dHash hamming distance at/above which a frame counts as "changed".
fn diff_threshold() -> u32 {
    env_u64("SCREEN_OBSERVE_DIFF_THRESHOLD", 6) as u32
}

/// Minimum seconds between narrator (VLM) calls — continuous change (video
/// playing) coalesces instead of firing per frame.
fn min_narrate_interval() -> Duration {
    Duration::from_secs(env_u64("SCREEN_OBSERVE_MIN_NARRATE_SECS", 15))
}

/// Stable-screen dwell before an optional deep-read pass.
pub fn deep_observe_dwell_s() -> u64 {
    env_u64("SCREEN_OBSERVE_DEEP_DWELL_SECS", 20)
}

/// Minimum repeat interval for deep-reading the same stable screen.
pub fn deep_observe_repeat_s() -> u64 {
    env_u64("SCREEN_OBSERVE_DEEP_REPEAT_SECS", 5 * 60)
}

/// Hard cap on observation length (minutes), regardless of config.
const MAX_MINUTES_CEILING: u64 = 480;
/// Default observation length when the caller doesn't say (minutes).
pub const DEFAULT_MAX_MINUTES: u64 = 120;

/// Wake phrases that mark a heard utterance as DIRECTLY ADDRESSED to the
/// observer (case-insensitive substring). Only addressed utterances drive
/// active screen correlation — ambient talk is still transcribed + summarized,
/// but does NOT nudge the vision narrator (which would otherwise invent links
/// between unrelated chatter and the screen). Override with
/// `SCREEN_OBSERVE_WAKE_PHRASES` (comma-separated); default
/// `hey magican,magican`.
fn observe_wake_phrases() -> &'static [String] {
    static PHRASES: OnceLock<Vec<String>> = OnceLock::new();
    PHRASES.get_or_init(|| {
        std::env::var("SCREEN_OBSERVE_WAKE_PHRASES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|p| p.trim().to_lowercase())
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| vec!["hey magican".to_string(), "magican".to_string()])
    })
}

/// True when an utterance addresses the observer by a wake phrase.
fn utterance_is_addressed(text: &str) -> bool {
    let lower = text.to_lowercase();
    observe_wake_phrases()
        .iter()
        .any(|phrase| lower.contains(phrase.as_str()))
}

// ─── config / status ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserveMode {
    /// Never interrupts: silent narration you read when you want.
    Notes,
    /// Narrates silently AND flags `watch_for` matches as alerts.
    Watch,
}

impl ObserveMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "watch" => Self::Watch,
            _ => Self::Notes,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Notes => "notes",
            Self::Watch => "watch",
        }
    }
}

/// Which audio the observation captures alongside the screen. System audio
/// rides the Screen-Recording grant the frame loop already holds (no extra
/// prompt); mic needs its own Microphone grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCaptureSource {
    None,
    System,
    Mic,
    Both,
}

impl AudioCaptureSource {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "system" | "display" => Self::System,
            "mic" | "microphone" => Self::Mic,
            "both" | "system+mic" | "all" => Self::Both,
            _ => Self::None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::System => "system",
            Self::Mic => "mic",
            Self::Both => "both",
        }
    }
    pub fn is_on(&self) -> bool {
        !matches!(self, Self::None)
    }
    pub fn wants_system(&self) -> bool {
        matches!(self, Self::System | Self::Both)
    }
    pub fn wants_mic(&self) -> bool {
        matches!(self, Self::Mic | Self::Both)
    }
}

/// Optional audio capture settings for an observation (off by default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserveAudioConfig {
    pub source: AudioCaptureSource,
    pub audio_profile: Option<String>,
    pub audio_stage_options: BTreeMap<AudioStage, String>,
}

impl Default for ObserveAudioConfig {
    fn default() -> Self {
        Self {
            source: AudioCaptureSource::None,
            audio_profile: None,
            audio_stage_options: BTreeMap::new(),
        }
    }
}

/// Session-fixed settings (cadence, caps, audio, where narration lands).
#[derive(Debug, Clone)]
pub struct ObserveConfig {
    pub cadence_s: u64,
    pub max_minutes: u64,
    /// UI thread the narration session lives under.
    pub thread: String,
    pub session_title: String,
    /// Optional audio capture alongside the frames.
    pub audio: ObserveAudioConfig,
}

/// What the observer is LOOKING FOR — mutable mid-session (`retarget`):
/// a notes session can upgrade to watch mode once the user says what to
/// watch for, and back.
#[derive(Debug, Clone)]
pub struct ObserveTarget {
    pub purpose: String,
    pub mode: ObserveMode,
    /// `watch` mode: the alert condition, in plain language.
    pub watch_for: Option<String>,
    /// Stop the session once an alert fires (default true in `watch` mode).
    pub stop_on_match: bool,
    /// Optional stable-screen deep-read pass: after the same screen remains
    /// visible for `deep_observe_dwell_s`, run one high-detail understanding
    /// pass and repeat at most every `deep_observe_repeat_s` while unchanged.
    pub deep_observation: bool,
}

/// Partial retarget — only provided fields change.
#[derive(Debug, Default, Clone)]
pub struct ObserveTargetUpdate {
    pub purpose: Option<String>,
    pub mode: Option<ObserveMode>,
    pub watch_for: Option<String>,
    pub stop_on_match: Option<bool>,
    pub deep_observation: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserveStatus {
    Observing,
    Stopped,
    Failed,
}

impl ObserveStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Observing => "observing",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ObserveStatusView {
    pub observe_id: String,
    pub status: ObserveStatus,
    pub purpose: String,
    pub mode: &'static str,
    pub watch_for: Option<String>,
    pub thread: String,
    pub started_at_ms: u64,
    pub note_count: usize,
    pub alert_count: usize,
    pub transcript_count: usize,
    pub audio_source: &'static str,
    pub audio_profile: Option<String>,
    pub stt_provider: Option<String>,
    pub deep_observation: bool,
    pub deep_dwell_s: u64,
    pub deep_repeat_s: u64,
    pub deep_note_count: usize,
    pub latest_summary: Option<String>,
}

// ─── narrator ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NarrationKind {
    Nothing,
    Note,
    Alert,
}

#[derive(Debug, Clone)]
pub struct NarrationVerdict {
    pub kind: NarrationKind,
    pub line: String,
}

/// One changed frame in → one verdict out. Implementations must be cheap to
/// clone behind an Arc and safe to call serially from the observe loop.
#[async_trait]
pub trait FrameNarrator: Send + Sync {
    /// `addressed` carries the recent wake-phrase utterances ("hey magican …")
    /// when the user has spoken TO the observer — empty on a plain screen-change
    /// pass. It turns the narration into a focused answer about the current
    /// screen; ambient talk is deliberately NOT passed (it would invent links).
    async fn narrate(
        &self,
        frame_png: &[u8],
        purpose: &str,
        watch_for: Option<&str>,
        recent_notes: &[String],
        addressed: &[String],
    ) -> Result<NarrationVerdict, String>;

    /// High-detail stable-screen read. This is optional and much less frequent
    /// than narration: it uses the full `screen_understanding` operation on a
    /// screen that has stayed visually stable long enough to deserve inspection.
    async fn deep_understand(
        &self,
        frame_png: &[u8],
        purpose: &str,
        watch_for: Option<&str>,
        recent_notes: &[String],
    ) -> Result<NarrationVerdict, String>;
}

/// Router-backed narrator: every frame routes through the
/// `screen_observation` operation in `magician-config.yaml` → a vision
/// profile (gpt-5.6-terra). The model lives in config like every other LLM
/// call — there are NO narrator env vars.
pub struct RouterFrameNarrator {
    router: Arc<magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter>,
    telemetry: Option<
        magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext,
    >,
    execution_id: Option<String>,
}

impl RouterFrameNarrator {
    /// Requires the process-global router (set at server startup). The rail
    /// refuses to start without it — a silently model-less observer would
    /// capture frames for nothing.
    pub fn from_global() -> Result<Self, String> {
        match magician::magician_v2::query_analysis::operation_llm_router::global_operation_router()
        {
            Some(router) => Ok(Self {
                router,
                telemetry: None,
                execution_id: None,
            }),
            None => Err(
                "screen observation requires the operation router (magician-config.yaml \
                 llm.router, operation `screen_observation`)"
                    .to_string(),
            ),
        }
    }

    pub fn from_global_with_telemetry(
        broadcaster: Option<
            Arc<magician::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
        >,
        principal: &str,
        workspace: &str,
        execution_id: &str,
    ) -> Result<Self, String> {
        let mut narrator = Self::from_global()?;
        narrator.router = Arc::new(
            narrator
                .router
                .with_scope_context(Some(magicllm::LlmScope::new(principal, workspace))),
        );
        narrator.telemetry = broadcaster.map(|broadcaster| {
            magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
                broadcaster,
                principal,
                workspace,
                "screen_observation",
            )
        });
        narrator.execution_id = Some(execution_id.to_string());
        Ok(narrator)
    }
}

fn parse_narration_verdict(text: &str) -> (NarrationVerdict, bool) {
    let cleaned = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let parsed_result = serde_json::from_str::<serde_json::Value>(cleaned);
    let schema_valid = parsed_result.as_ref().is_ok_and(|parsed| {
        matches!(
            parsed.get("verdict").and_then(|value| value.as_str()),
            Some("nothing" | "note" | "alert")
        ) && parsed
            .get("line")
            .and_then(|value| value.as_str())
            .is_some()
    });
    let parsed = parsed_result.unwrap_or_else(|_| json!({ "verdict": "note", "line": cleaned }));
    let kind = match parsed
        .get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("nothing")
    {
        "alert" => NarrationKind::Alert,
        "note" => NarrationKind::Note,
        _ => NarrationKind::Nothing,
    };
    let line = parsed
        .get("line")
        .and_then(|l| l.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    (NarrationVerdict { kind, line }, schema_valid)
}

#[async_trait]
impl FrameNarrator for RouterFrameNarrator {
    async fn narrate(
        &self,
        frame_png: &[u8],
        purpose: &str,
        watch_for: Option<&str>,
        recent_notes: &[String],
        addressed: &[String],
    ) -> Result<NarrationVerdict, String> {
        use magician::magician_v2::query_analysis::operation_llm_router::LLMOperation;
        use magician::magician_v2::slot_graph::extraction::{ImageData, ImageDetail};

        // Store-managed prompt (data/magician_v2/prompts/); the literal is
        // the degrade-loudly fallback, not the source of truth.
        let alert_section = watch_for
            .map(|condition| {
                format!("ALERT CONDITION — the user wants to be alerted when: {condition}")
            })
            .unwrap_or_default();
        let fallback = format!(
            "You are silently observing the user's computer screen.\n\
             Purpose of this observation: {purpose}\n\
             {alert_section}\n\
             You receive ONE new screenshot (the screen just changed). Recent notes you \
             already made are provided for continuity — do not repeat them.\n\
             Respond with STRICT JSON only: {{\"verdict\": \"nothing\"|\"note\"|\"alert\", \
             \"line\": \"...\"}}.\n\
             - \"nothing\": trivial change (cursor, clock, tiny scroll) — empty line.\n\
             - \"note\": something worth one line of a work journal. The line is ONE short \
             plain sentence about what is happening on screen.\n\
             - \"alert\": ONLY when the alert condition above is clearly met. The line \
             states what matched.\n\
             Never use \"alert\" when no alert condition was given."
        );
        let mut variables = std::collections::HashMap::new();
        variables.insert("purpose".to_string(), purpose.to_string());
        variables.insert("alert_condition_section".to_string(), alert_section);
        let system = magician::magician_v2::prompts::rendered_prompt_or(
            magician::magician_v2::prompts::names::SCREEN_OBSERVATION_SYSTEM,
            magician::magician_v2::prompts::versions::SCREEN_OBSERVATION_SYSTEM,
            variables,
            &fallback,
        )
        .await;
        let context = if recent_notes.is_empty() {
            "(none yet)".to_string()
        } else {
            recent_notes.join("\n")
        };
        // Active correlation is WAKE-WORD GATED: the narrator only sees speech
        // the user aimed at it (not ambient talk), so it answers a real question
        // about the screen instead of inventing links to background chatter.
        let addressed_section = if addressed.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nThe user just spoke TO you (by name):\n{}\n\n\
                 Answer their question about the CURRENT screen in ONE short \
                 sentence (verdict: \"note\"). If the screen does not actually \
                 relate to what they asked, say so plainly — do NOT invent a \
                 connection.",
                addressed.join("\n")
            )
        };
        let user_prompt =
            format!("Recent notes:\n{context}{addressed_section}\n\nThe new frame is attached.");
        // detail: Low — narration needs the gist, not pixel-perfect OCR; it
        // keeps the per-frame token cost flat regardless of display size.
        let image = ImageData::with_detail(
            base64::engine::general_purpose::STANDARD.encode(frame_png),
            "image/png".to_string(),
            ImageDetail::Low,
        );
        let llm_started = std::time::Instant::now();
        let response = self
            .router
            .generate_for_execution_native_tools(
                &LLMOperation::ScreenObservation,
                Some(&system),
                &user_prompt,
                Vec::new(),
                None,
                Some(std::slice::from_ref(&image)),
                None,
                None,
            )
            .await
            .map_err(|e| format!("narrator (screen_observation operation): {e}"))?;
        let (verdict, schema_valid) =
            parse_narration_verdict(response.text.as_deref().unwrap_or(""));
        if let Some(telemetry) = self.telemetry.as_ref() {
            let attribution = magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution {
                execution_id: self.execution_id.clone(),
                ..Default::default()
            };
            if schema_valid {
                telemetry.emit_native_validated_success(
                    LLMOperation::ScreenObservation.as_str(),
                    &response,
                    llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    attribution,
                    "screen_narration_verdict",
                );
            } else {
                telemetry.emit_native_validation_failure(
                    LLMOperation::ScreenObservation.as_str(),
                    &response,
                    llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    attribution,
                    "screen_narration_verdict",
                    "response did not match the narration verdict schema",
                );
            }
        }
        Ok(verdict)
    }

    async fn deep_understand(
        &self,
        frame_png: &[u8],
        purpose: &str,
        watch_for: Option<&str>,
        recent_notes: &[String],
    ) -> Result<NarrationVerdict, String> {
        use magician::magician_v2::query_analysis::operation_llm_router::LLMOperation;
        use magician::magician_v2::slot_graph::extraction::{ImageData, ImageDetail};

        let alert_section = watch_for
            .map(|condition| {
                format!("ALERT CONDITION — the user wants to be alerted when: {condition}")
            })
            .unwrap_or_default();
        let fallback = format!(
            "You are doing a deep observation pass on the user's computer screen.\n\
             Purpose of this observation: {purpose}\n\
             {alert_section}\n\
             The screen has remained visually stable long enough that the user may \
             be reading, debugging, waiting, or inspecting details.\n\
             Use the attached screenshot carefully: read visible text, UI state, \
             errors, progress, selected items, and subtle status indicators.\n\
             Recent notes are provided for continuity. Do not repeat them unless \
             the current frame adds a concrete new detail.\n\
             Respond with STRICT JSON only: {{\"verdict\": \"nothing\"|\"note\"|\"alert\", \
             \"line\": \"...\"}}.\n\
             Never use \"alert\" when no alert condition was given. Do not speculate \
             beyond what is visible."
        );
        let mut variables = std::collections::HashMap::new();
        variables.insert("purpose".to_string(), purpose.to_string());
        variables.insert("alert_condition_section".to_string(), alert_section);
        let system = magician::magician_v2::prompts::rendered_prompt_or(
            magician::magician_v2::prompts::names::SCREEN_DEEP_OBSERVATION_SYSTEM,
            magician::magician_v2::prompts::versions::SCREEN_DEEP_OBSERVATION_SYSTEM,
            variables,
            &fallback,
        )
        .await;
        let context = if recent_notes.is_empty() {
            "(none yet)".to_string()
        } else {
            recent_notes.join("\n")
        };
        let user_prompt = format!(
            "Recent notes:\n{context}\n\nThe current stable frame is attached. Return only a new detail worth recording, or nothing."
        );
        let image = ImageData::with_detail(
            base64::engine::general_purpose::STANDARD.encode(frame_png),
            "image/png".to_string(),
            ImageDetail::High,
        );
        let llm_started = std::time::Instant::now();
        let response = self
            .router
            .generate_for_execution_native_tools(
                &LLMOperation::ScreenUnderstanding,
                Some(&system),
                &user_prompt,
                Vec::new(),
                None,
                Some(std::slice::from_ref(&image)),
                None,
                None,
            )
            .await
            .map_err(|e| format!("deep narrator (screen_understanding operation): {e}"))?;
        let (verdict, schema_valid) =
            parse_narration_verdict(response.text.as_deref().unwrap_or(""));
        if let Some(telemetry) = self.telemetry.as_ref() {
            let attribution = magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution {
                execution_id: self.execution_id.clone(),
                ..Default::default()
            };
            if schema_valid {
                telemetry.emit_native_validated_success(
                    LLMOperation::ScreenUnderstanding.as_str(),
                    &response,
                    llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    attribution,
                    "screen_narration_verdict",
                );
            } else {
                telemetry.emit_native_validation_failure(
                    LLMOperation::ScreenUnderstanding.as_str(),
                    &response,
                    llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    attribution,
                    "screen_narration_verdict",
                    "response did not match the narration verdict schema",
                );
            }
        }
        Ok(verdict)
    }
}

// ─── frame capture + dHash gate ─────────────────────────────────────────

/// Resolve a CLI that may live outside the server PATH (same shape as the
/// helper in `api/screen_api.rs`; duplicated to keep the rail free of
/// api-layer imports).
fn resolve_bin(name: &str, fallbacks: &[&str]) -> String {
    for candidate in fallbacks {
        let expanded = if let Some(rest) = candidate.strip_prefix("~/") {
            match std::env::var("HOME") {
                Ok(home) => format!("{home}/{rest}"),
                Err(_) => continue,
            }
        } else {
            (*candidate).to_string()
        };
        if Path::new(&expanded).exists() {
            return expanded;
        }
    }
    name.to_string()
}

fn ffmpeg_bin() -> String {
    resolve_bin(
        "ffmpeg",
        &["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"],
    )
}

/// Capture a full-display PNG. macOS screen capture runs ONLY on the host (it
/// needs the desktop app's Screen-Recording TCC grant), so this ALWAYS relays
/// through the Tauri host gateway — one path for native AND container. There is
/// no local `screencapture` spawn: native screen-observation requires the
/// desktop app running (the cost of a single code path).
async fn capture_frame() -> Result<Vec<u8>, String> {
    let provider = HostAutomationProvider::new(host_gateway_url_from_env());
    let capture = provider
        .capture_screen(ScreenCaptureRequest::default())
        .await
        .map_err(|e| format!("host screen capture: {e}"))?;
    if capture.image.is_empty() {
        return Err("empty frame".into());
    }
    Ok(capture.image)
}

/// 64-bit dHash of a PNG via ffmpeg (no image crate in the workspace):
/// downscale to 9×8 grayscale raw, then bit r*8+c = px[r][c] < px[r][c+1].
async fn dhash_png(png: &[u8]) -> Result<u64, String> {
    let tmp = std::env::temp_dir().join(format!(
        "magician-observe-hash-{}.png",
        uuid::Uuid::new_v4().simple()
    ));
    tokio::fs::write(&tmp, png)
        .await
        .map_err(|e| format!("write hash tmp: {e}"))?;
    let output = Command::new(ffmpeg_bin())
        .arg("-v")
        .arg("error")
        .arg("-i")
        .arg(&tmp)
        .arg("-vf")
        .arg("scale=9:8")
        .arg("-pix_fmt")
        .arg("gray")
        .arg("-f")
        .arg("rawvideo")
        .arg("-")
        .output()
        .await
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;
    let _ = tokio::fs::remove_file(&tmp).await;
    if !output.status.success() || output.stdout.len() < 72 {
        return Err(format!(
            "ffmpeg dhash failed ({}; {} bytes out)",
            output.status,
            output.stdout.len()
        ));
    }
    let px = &output.stdout[..72];
    let mut hash: u64 = 0;
    for row in 0..8 {
        for col in 0..8 {
            if px[row * 9 + col] < px[row * 9 + col + 1] {
                hash |= 1 << (row * 8 + col);
            }
        }
    }
    Ok(hash)
}

fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

// ─── memory append ──────────────────────────────────────────────────────

/// One bounded provenance entry per observation in the SAME tier the
/// one-shot captures use (`user.screen_observations`, `observe:` prefix
/// shares the `screen:`-side retention budget via its own prefix rule).
async fn append_observe_memory(
    resources: &Arc<AgentResources>,
    principal: &str,
    workspace: &str,
    observe_id: &str,
    target: &ObserveTarget,
    note_count: usize,
    alert_count: usize,
    transcript_count: usize,
    summary: Option<&str>,
) {
    use magician::magician_v2::chat::service::{
        merge_user_memory_tier_fields_with_retention, normalized_user_memory_tier_name,
    };
    let Some(tier) = normalized_user_memory_tier_name("user.screen_observations") else {
        return;
    };
    if tier.is_empty() {
        return;
    }
    let now = chrono::Local::now();
    let entry_key = format!("observe:{observe_id}");
    let mut entry = serde_json::Map::new();
    entry.insert("key".into(), json!(entry_key));
    entry.insert("source_type".into(), json!("screen_observation"));
    entry.insert("purpose".into(), json!(target.purpose));
    entry.insert("mode".into(), json!(target.mode.as_str()));
    entry.insert("date".into(), json!(now.format("%Y-%m-%d").to_string()));
    entry.insert("time".into(), json!(now.format("%H:%M").to_string()));
    entry.insert("notes".into(), json!(note_count));
    entry.insert("alerts".into(), json!(alert_count));
    if transcript_count > 0 {
        entry.insert("transcript_lines".into(), json!(transcript_count));
    }
    if let Some(summary) = summary {
        entry.insert("summary".into(), json!(summary));
    }
    let mut tier_fields = serde_json::Map::new();
    tier_fields.insert(
        entry
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        serde_json::Value::Object(entry),
    );
    let result = merge_user_memory_tier_fields_with_retention(
        resources.memory_resolver.as_ref(),
        principal,
        workspace,
        &tier,
        &tier_fields,
        Some(("observe:", 20)),
    )
    .await;
    tracing::debug!(
        target: "screen_observe",
        ?result,
        "observation provenance appended to memory tier {tier}"
    );
}

// ─── the session ────────────────────────────────────────────────────────

struct ObserveState {
    status: ObserveStatus,
    notes: Vec<String>,
    alert_count: usize,
    /// Heard-audio transcript finals (speaker-prefixed), kept separate from the
    /// screen `notes` so the narrator's frame context isn't diluted; both are
    /// merged at teardown for the summary + memory. Bounded (front-evicted).
    transcript: Vec<String>,
    /// Monotonic count of audio finals ever recorded (status/memory count).
    transcript_total: usize,
    /// Utterances that ADDRESSED the observer by wake phrase ("hey magican …").
    /// Only these drive active screen correlation; ambient talk never reaches
    /// the narrator. Bounded (front-evicted).
    addressed: Vec<String>,
    /// Monotonic count of addressed utterances — the correlation trigger's
    /// watermark (NOT `addressed.len()`, which saturates at the cap).
    addressed_total: usize,
    /// Number of stable-screen deep-read notes posted during the session.
    deep_note_count: usize,
    resolved_audio_profile: Option<String>,
    resolved_stt_provider: Option<String>,
    latest_summary: Option<String>,
}

/// Where the observation loop gets its frames. `Host` captures the operator's
/// screen via the host gateway (the Mac / web control-plane case). `Pushed`
/// reads the newest frame a remote client streamed in (the iOS broadcast
/// extension) — the loop still ticks on `cadence_s`, `take()`ing the latest
/// pushed frame; a tick with nothing new is skipped (no model call), so the
/// diff-gate's cost/privacy properties are preserved.
enum FrameSource {
    Host,
    Pushed(Arc<std::sync::Mutex<Option<Vec<u8>>>>),
}

pub struct ScreenObserveSession {
    observe_id: String,
    config: ObserveConfig,
    target: Mutex<ObserveTarget>,
    narrator: Arc<dyn FrameNarrator>,
    summarizer: Arc<dyn Summarizer>,
    sink: Arc<dyn TranscriptSink>,
    resources: Arc<AgentResources>,
    principal: String,
    workspace: String,
    frame_source: FrameSource,
    cancel: CancellationToken,
    paused: Arc<AtomicBool>,
    started_at_ms: u64,
    state: Mutex<ObserveState>,
}

impl ScreenObserveSession {
    #[allow(clippy::too_many_arguments)]
    fn new(
        observe_id: String,
        config: ObserveConfig,
        target: ObserveTarget,
        narrator: Arc<dyn FrameNarrator>,
        summarizer: Arc<dyn Summarizer>,
        sink: Arc<dyn TranscriptSink>,
        resources: Arc<AgentResources>,
        principal: String,
        workspace: String,
        frame_source: FrameSource,
    ) -> Self {
        Self {
            observe_id,
            config,
            target: Mutex::new(target),
            narrator,
            summarizer,
            sink,
            resources,
            principal,
            workspace,
            frame_source,
            cancel: CancellationToken::new(),
            paused: Arc::new(AtomicBool::new(false)),
            started_at_ms: chrono::Utc::now().timestamp_millis() as u64,
            state: Mutex::new(ObserveState {
                status: ObserveStatus::Observing,
                notes: Vec::new(),
                alert_count: 0,
                transcript: Vec::new(),
                transcript_total: 0,
                addressed: Vec::new(),
                addressed_total: 0,
                deep_note_count: 0,
                resolved_audio_profile: None,
                resolved_stt_provider: None,
                latest_summary: None,
            }),
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    /// Acquire the next frame for a loop tick. `Ok(Some)` = a frame to analyze,
    /// `Ok(None)` = a pushed source with nothing new this tick (skip, no cost),
    /// `Err` = a real capture failure (host source).
    async fn next_frame(&self) -> Result<Option<Vec<u8>>, String> {
        match &self.frame_source {
            FrameSource::Host => capture_frame().await.map(Some),
            FrameSource::Pushed(latest) => Ok(latest.lock().unwrap().take()),
        }
    }

    pub async fn status_view(&self) -> ObserveStatusView {
        let target = self.target.lock().await.clone();
        let st = self.state.lock().await;
        ObserveStatusView {
            observe_id: self.observe_id.clone(),
            status: st.status,
            purpose: target.purpose,
            mode: target.mode.as_str(),
            watch_for: target.watch_for,
            thread: self.config.thread.clone(),
            started_at_ms: self.started_at_ms,
            note_count: st.notes.len(),
            alert_count: st.alert_count,
            transcript_count: st.transcript_total,
            audio_source: self.config.audio.source.as_str(),
            audio_profile: st.resolved_audio_profile.clone(),
            stt_provider: st.resolved_stt_provider.clone(),
            deep_observation: target.deep_observation,
            deep_dwell_s: deep_observe_dwell_s(),
            deep_repeat_s: deep_observe_repeat_s(),
            deep_note_count: st.deep_note_count,
            latest_summary: st.latest_summary.clone(),
        }
    }

    /// Mid-session retarget: only provided fields change. A notes session
    /// upgrades to `watch` the moment a condition arrives; `watch → notes`
    /// drops alerting but keeps narrating. The change is announced in the
    /// session so the record shows when the goal shifted.
    pub async fn retarget(&self, update: ObserveTargetUpdate) -> Result<(), String> {
        let announced = {
            let mut target = self.target.lock().await;
            if let Some(purpose) = update.purpose {
                target.purpose = purpose;
            }
            if let Some(mode) = update.mode {
                target.mode = mode;
            }
            if let Some(watch_for) = update.watch_for {
                target.watch_for = Some(watch_for);
                // A condition arriving without an explicit mode implies watch.
                if update.mode.is_none() {
                    target.mode = ObserveMode::Watch;
                }
            }
            if let Some(stop_on_match) = update.stop_on_match {
                target.stop_on_match = stop_on_match;
            }
            if let Some(deep_observation) = update.deep_observation {
                target.deep_observation = deep_observation;
            }
            if target.mode == ObserveMode::Watch && target.watch_for.is_none() {
                return Err("watch mode requires a watch_for condition".to_string());
            }
            let deep_suffix = if update.deep_observation.is_some() {
                if target.deep_observation {
                    " · deep read on"
                } else {
                    " · deep read off"
                }
            } else {
                ""
            };
            match (target.mode, target.watch_for.as_deref()) {
                (ObserveMode::Watch, Some(condition)) => {
                    format!("🎯 retargeted — alert when: {condition}{deep_suffix}")
                },
                _ => format!(
                    "🎯 retargeted — notes mode ({}){deep_suffix}",
                    target.purpose
                ),
            }
        };
        self.post("Observer", announced).await;
        Ok(())
    }

    async fn post(&self, speaker: &str, text: String) {
        self.sink
            .post_turn(&TranscriptTurn {
                at_ms: 0,
                speaker: Some(speaker.to_string()),
                text,
            })
            .await;
    }

    /// Record one finalized audio utterance: post it live into the session and
    /// keep it for the teardown summary + memory (bounded, like `notes`).
    /// macOS-only — only the audio drain (also macOS-gated) calls it.
    #[cfg(target_os = "macos")]
    async fn record_heard(&self, speaker: &str, text: String) {
        let line = text.trim().to_string();
        if line.is_empty() {
            return;
        }
        self.post(speaker, line.clone()).await;
        let mut st = self.state.lock().await;
        // Keep the speaker label inline (🔊 System / 🎙 You) so the teardown
        // summarizer can tell the two audio sources apart — the summary input
        // also splits screen-vs-audio with section headers.
        st.transcript.push(format!("{speaker}: {line}"));
        st.transcript_total += 1;
        if st.transcript.len() > 400 {
            st.transcript.remove(0);
        }
        // Wake-phrase gate: only an utterance that addresses the observer drives
        // active correlation. Ambient talk above is still recorded + summarized,
        // but never reaches the narrator (no invented screen↔chatter links).
        if utterance_is_addressed(&line) {
            st.addressed.push(format!("{speaker}: {line}"));
            st.addressed_total += 1;
            if st.addressed.len() > 16 {
                st.addressed.remove(0);
            }
        }
    }

    /// Spawn the optional audio capture→STT track(s). macOS-only (the capture
    /// bridge is); elsewhere it posts a one-line note so the gap is loud, not
    /// silent.
    #[cfg(target_os = "macos")]
    fn spawn_audio_tracks(self: &Arc<Self>) {
        let audio = self.config.audio.clone();
        if !audio.source.is_on() {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            this.spawn_audio_tracks_inner(audio).await;
        });
    }

    #[cfg(not(target_os = "macos"))]
    fn spawn_audio_tracks(self: &Arc<Self>) {
        if self.config.audio.source.is_on() {
            let this = self.clone();
            tokio::spawn(async move {
                this.post(
                    "Observer",
                    "Audio capture is macOS-only for now; observing screen frames only."
                        .to_string(),
                )
                .await;
            });
        }
    }

    /// Resolve the STT provider for the session's choice and spawn one
    /// capture→STT pump per requested source. Each failure (missing helper,
    /// missing key, capture error) is posted into the session rather than
    /// silently dropped — the screen narration keeps running regardless.
    #[cfg(target_os = "macos")]
    async fn spawn_audio_tracks_inner(self: Arc<Self>, audio: ObserveAudioConfig) {
        use crate::media_rails::meeting::bridge_macos::{
            MicrophoneAudioSource, ScreenCaptureAudioSource,
        };
        use crate::media_rails::meeting::{AudioSource, CaptureTarget};

        let (provider, profile) =
            match crate::media_rails::resolve_installed_surface_audio_pipeline(
                crate::media_rails::AudioSurface::Listening,
                Some((self.principal.clone(), self.workspace.clone())),
                audio.audio_profile.as_deref(),
                &audio.audio_stage_options,
                true,
            )
            .await
            {
                Ok(provider) => provider,
                Err(err) => {
                    self.post(
                        "Observer",
                        format!("Audio capture skipped — audio profile unavailable ({err})."),
                    )
                    .await;
                    return;
                },
            };
        {
            let selected = profile
                .stages
                .get(&AudioStage::StreamingStt)
                .and_then(|stage| stage.selected.as_ref())
                .map(|option| option.provider_id.clone());
            let mut state = self.state.lock().await;
            state.resolved_audio_profile = Some(profile.profile_id);
            state.resolved_stt_provider = selected;
        }

        if audio.source.wants_system() {
            let system = Arc::new(ScreenCaptureAudioSource::new());
            // Whole-display system audio — the cloak-proven target that hears
            // every app, not just one bundle.
            system.set_capture_target(CaptureTarget::DisplayAudio).await;
            self.clone()
                .spawn_one_audio_track(
                    provider.clone(),
                    system as Arc<dyn AudioSource>,
                    "🔊 System",
                )
                .await;
        }
        if audio.source.wants_mic() {
            let mic = Arc::new(MicrophoneAudioSource::new()) as Arc<dyn AudioSource>;
            self.clone()
                .spawn_one_audio_track(provider.clone(), mic, "🎙 You")
                .await;
        }
    }

    /// Wire one source → STT session → sink: a capture task (cancellable), a
    /// pump that drops chunks while paused and finishes the recognizer when
    /// capture ends, and a drain that records each final utterance.
    #[cfg(target_os = "macos")]
    async fn spawn_one_audio_track(
        self: Arc<Self>,
        provider: Arc<dyn crate::media_rails::providers::StreamingSttProvider>,
        source: Arc<dyn crate::media_rails::meeting::AudioSource>,
        speaker: &'static str,
    ) {
        use crate::media_rails::providers::{AudioChunk, StreamAudioFormat, StreamingSttEvent};
        use tokio::sync::mpsc;

        let (events_tx, mut events_rx) = mpsc::channel::<StreamingSttEvent>(64);
        let stt_session = match provider
            .open_session(StreamAudioFormat::default(), events_tx)
            .await
        {
            Ok(session) => session,
            Err(err) => {
                self.post(
                    "Observer",
                    format!(
                        "{speaker}: speech-to-text unavailable ({err}). This source was not captured."
                    ),
                )
                .await;
                return;
            },
        };

        // Capture: stream chunks until cancelled or the source ends.
        let (chunk_tx, mut chunk_rx) = mpsc::channel::<AudioChunk>(64);
        {
            let cancel = self.cancel.clone();
            let this = self.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    result = source.run(chunk_tx) => {
                        if let Err(err) = result {
                            this.post("Observer", format!("{speaker}: audio capture ended ({err}).")).await;
                        }
                    }
                }
            });
        }

        // Pump: drop chunks while paused (nothing transcribed); finish() on end
        // so the recognizer flushes its final utterance.
        {
            let paused = self.paused.clone();
            tokio::spawn(async move {
                while let Some(chunk) = chunk_rx.recv().await {
                    if paused.load(Ordering::SeqCst) {
                        continue;
                    }
                    if stt_session.push_audio(chunk).await.is_err() {
                        break;
                    }
                }
                let _ = stt_session.finish().await;
            });
        }

        // Drain: each finalized utterance becomes a live turn + summary input.
        {
            let this = self.clone();
            tokio::spawn(async move {
                while let Some(event) = events_rx.recv().await {
                    match event {
                        StreamingSttEvent::Final { text, .. } => {
                            this.record_heard(speaker, text).await;
                        },
                        StreamingSttEvent::Error { reason } => {
                            tracing::warn!(target: "screen_observe", %reason, "observe audio STT error");
                        },
                        StreamingSttEvent::Partial { .. } => {},
                    }
                }
            });
        }
    }

    async fn run(self: Arc<Self>) {
        // Optional audio rail: spawn capture→STT track(s) that post finals into
        // the same session sink alongside the frame narration.
        self.spawn_audio_tracks();
        let cadence = Duration::from_secs(self.config.cadence_s.max(2));
        let max_duration =
            Duration::from_secs(self.config.max_minutes.clamp(1, MAX_MINUTES_CEILING) * 60);
        // An audio session legitimately sits on a static screen (a call in a
        // background window): the screen-change idle-stop would kill it early,
        // so only the max-minutes cap / explicit stop end an audio observation.
        let idle_limit = if self.config.audio.source.is_on() {
            None
        } else {
            idle_stop()
        };
        let narrate_gap = min_narrate_interval();
        let deep_dwell = Duration::from_secs(deep_observe_dwell_s().max(1));
        let deep_repeat = Duration::from_secs(deep_observe_repeat_s().max(1));
        let threshold = diff_threshold();
        // When audio is on, an ADDRESSED utterance ("hey magican …") since the
        // last narration also triggers a pass (wake-word-gated correlation) —
        // ambient talk is recorded + summarized but never drives the narrator,
        // so it can't invent links between background chatter and the screen.
        let audio_on = self.config.audio.source.is_on();
        let started = Instant::now();
        let mut last_hash: Option<u64> = None;
        let mut stable_since = Instant::now();
        let mut last_change = Instant::now();
        let mut last_narrate = Instant::now() - narrate_gap;
        let mut last_deep_hash: Option<u64> = None;
        let mut last_deep_at: Option<Instant> = None;
        let mut last_narrated_addressed_total: usize = 0;
        let mut stop_note: Option<String> = None;
        let mut consecutive_failures: u32 = 0;

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => break,
                _ = tokio::time::sleep(cadence) => {}
            }
            if self.paused.load(Ordering::SeqCst) {
                // Paused time counts as neither change nor idleness.
                last_change = Instant::now();
                continue;
            }
            if started.elapsed() >= max_duration {
                stop_note = Some(format!(
                    "Observation stopped automatically at the {}-minute limit.",
                    max_duration.as_secs() / 60
                ));
                break;
            }
            if let Some(limit) = idle_limit {
                if last_change.elapsed() >= limit {
                    stop_note = Some(format!(
                        "Observation stopped automatically after {} minutes without screen changes.",
                        limit.as_secs() / 60
                    ));
                    break;
                }
            }
            let frame = match self.next_frame().await {
                Ok(Some(frame)) => {
                    consecutive_failures = 0;
                    frame
                },
                // Pushed source with no new frame this tick — not a failure; the
                // static-screen idle-stop still governs an ended/paused stream.
                Ok(None) => continue,
                Err(error) => {
                    consecutive_failures += 1;
                    tracing::warn!(target: "screen_observe", %error, "frame capture failed");
                    if consecutive_failures >= 5 {
                        stop_note = Some(
                            "Observation stopped: screen capture kept failing (check the Screen Recording permission).".to_string(),
                        );
                        self.state.lock().await.status = ObserveStatus::Failed;
                        break;
                    }
                    continue;
                },
            };
            let hash = match dhash_png(&frame).await {
                Ok(hash) => hash,
                Err(error) => {
                    tracing::warn!(target: "screen_observe", %error, "frame hash failed");
                    continue;
                },
            };
            let screen_changed = match last_hash {
                None => true,
                Some(previous) => hamming(previous, hash) >= threshold,
            };
            if screen_changed {
                stable_since = Instant::now();
                last_deep_hash = None;
                last_deep_at = None;
            }
            // Active correlation trigger: only a NEW addressed utterance ("hey
            // presto …") warrants an audio-driven pass — ambient talk does not.
            let addressed_total_now = if audio_on {
                self.state.lock().await.addressed_total
            } else {
                0
            };
            let addressed_new = addressed_total_now > last_narrated_addressed_total;
            if !screen_changed && !addressed_new {
                let target = self.target.lock().await.clone();
                let deep_hash = last_hash.unwrap_or(hash);
                let deep_due = target.deep_observation
                    && stable_since.elapsed() >= deep_dwell
                    && (last_deep_hash != Some(deep_hash)
                        || last_deep_at
                            .map(|at| at.elapsed() >= deep_repeat)
                            .unwrap_or(true));
                if deep_due {
                    last_deep_hash = Some(deep_hash);
                    last_deep_at = Some(Instant::now());
                    let recent: Vec<String> = {
                        let st = self.state.lock().await;
                        st.notes.iter().rev().take(8).rev().cloned().collect()
                    };
                    let verdict = match self
                        .narrator
                        .deep_understand(
                            &frame,
                            &target.purpose,
                            target.watch_for.as_deref(),
                            &recent,
                        )
                        .await
                    {
                        Ok(verdict) => verdict,
                        Err(error) => {
                            tracing::warn!(target: "screen_observe", %error, "deep narrator failed");
                            continue;
                        },
                    };
                    let kind = if verdict.kind == NarrationKind::Alert
                        && target.mode != ObserveMode::Watch
                    {
                        NarrationKind::Note
                    } else {
                        verdict.kind
                    };
                    match kind {
                        NarrationKind::Nothing => {},
                        NarrationKind::Note => {
                            if !verdict.line.is_empty() {
                                self.post("🔎 Deep read", verdict.line.clone()).await;
                                let mut st = self.state.lock().await;
                                st.deep_note_count += 1;
                                st.notes.push(format!("DEEP: {}", verdict.line));
                                if st.notes.len() > 200 {
                                    st.notes.remove(0);
                                }
                            }
                        },
                        NarrationKind::Alert => {
                            let line = if verdict.line.is_empty() {
                                "watch condition matched".to_string()
                            } else {
                                verdict.line
                            };
                            self.post("👁 ALERT", line.clone()).await;
                            {
                                let mut st = self.state.lock().await;
                                st.alert_count += 1;
                                st.notes.push(format!("ALERT: {line}"));
                            }
                            tracing::info!(target: "screen_observe", %line, "deep observation alert");
                            if target.stop_on_match {
                                stop_note = Some(
                                    "Observation stopped: the watch condition matched.".to_string(),
                                );
                                break;
                            }
                        },
                    }
                }
                continue; // stable screen: optionally deep-read, otherwise no model call.
            }
            if screen_changed {
                last_change = Instant::now();
            }
            if last_narrate.elapsed() < narrate_gap {
                // Coalesce continuous change (video playing): keep the newest hash
                // so the next eligible tick compares against it. An addressed-audio
                // trigger waits out the same gap too — bounded cost.
                if screen_changed {
                    last_hash = Some(hash);
                }
                continue;
            }
            last_hash = Some(hash);
            last_narrate = Instant::now();
            // Snapshot recent notes + (only when this pass was triggered by a new
            // addressed utterance) the recent questions, then advance the watermark
            // so an answered question doesn't re-trigger.
            let (recent, addressed): (Vec<String>, Vec<String>) = {
                let st = self.state.lock().await;
                let notes = st.notes.iter().rev().take(6).rev().cloned().collect();
                let addressed = if addressed_new {
                    st.addressed.iter().rev().take(4).rev().cloned().collect()
                } else {
                    Vec::new()
                };
                (notes, addressed)
            };
            last_narrated_addressed_total = addressed_total_now;
            // Does this pass answer a question (vs post a passive screen note)?
            let responding = addressed_new;
            // Snapshot the (retargetable) goal for this narration pass.
            let target = self.target.lock().await.clone();
            let verdict = match self
                .narrator
                .narrate(
                    &frame,
                    &target.purpose,
                    target.watch_for.as_deref(),
                    &recent,
                    &addressed,
                )
                .await
            {
                Ok(verdict) => verdict,
                Err(error) => {
                    tracing::warn!(target: "screen_observe", %error, "narrator failed");
                    continue;
                },
            };
            // A model that "alerts" with no armed condition is misbehaving —
            // record it as a plain note instead of crying wolf.
            let kind = if verdict.kind == NarrationKind::Alert && target.mode != ObserveMode::Watch
            {
                NarrationKind::Note
            } else {
                verdict.kind
            };
            match kind {
                NarrationKind::Nothing => {},
                NarrationKind::Note => {
                    if !verdict.line.is_empty() {
                        // Channel marker: 💬 Presto = an answer to a wake-word
                        // question; 👁 Screen = a passive screen-understanding
                        // note. Both greppable, distinct when interleaved with
                        // 🔊 System / 🎙 You audio lines.
                        let speaker = if responding {
                            "💬 Presto"
                        } else {
                            "👁 Screen"
                        };
                        self.post(speaker, verdict.line.clone()).await;
                        let mut st = self.state.lock().await;
                        st.notes.push(verdict.line);
                        // Bound the in-memory journal (the thread holds the full record).
                        if st.notes.len() > 200 {
                            st.notes.remove(0);
                        }
                    }
                },
                NarrationKind::Alert => {
                    let line = if verdict.line.is_empty() {
                        "watch condition matched".to_string()
                    } else {
                        verdict.line
                    };
                    self.post("👁 ALERT", line.clone()).await;
                    {
                        let mut st = self.state.lock().await;
                        st.alert_count += 1;
                        st.notes.push(format!("ALERT: {line}"));
                    }
                    tracing::info!(target: "screen_observe", %line, "observation alert");
                    if target.stop_on_match {
                        stop_note =
                            Some("Observation stopped: the watch condition matched.".to_string());
                        break;
                    }
                },
            }
        }

        self.cancel.cancel();

        if let Some(note) = stop_note {
            self.post("Observer", note).await;
        }

        // Teardown: summarize the narration + any heard audio transcript (not
        // frames) + memory append.
        let (notes, alert_count, transcript, transcript_total) = {
            let st = self.state.lock().await;
            (
                st.notes.clone(),
                st.alert_count,
                st.transcript.clone(),
                st.transcript_total,
            )
        };
        let summary_input = if transcript.is_empty() {
            notes.join("\n")
        } else if notes.is_empty() {
            format!("Heard (audio transcript):\n{}", transcript.join("\n"))
        } else {
            format!(
                "Screen notes:\n{}\n\nHeard (audio transcript):\n{}",
                notes.join("\n"),
                transcript.join("\n")
            )
        };
        let summary = if notes.is_empty() && transcript.is_empty() {
            None
        } else {
            match self.summarizer.summarize(&summary_input).await {
                Ok(summary) => Some(summary),
                Err(error) => {
                    tracing::warn!(target: "screen_observe", %error, "observation summary failed");
                    // A silent skip reads as "the rail doesn't summarize" —
                    // say so in the thread; the notes above remain the record.
                    self.post(
                        "Observer",
                        format!(
                            "Summary unavailable (model error: {error}). The notes above are the full record."
                        ),
                    )
                    .await;
                    None
                },
            }
        };
        if let Some(summary) = summary.as_deref() {
            self.post("Observation summary", summary.to_string()).await;
        }
        let final_target = self.target.lock().await.clone();
        append_observe_memory(
            &self.resources,
            &self.principal,
            &self.workspace,
            &self.observe_id,
            &final_target,
            notes.len(),
            alert_count,
            transcript_total,
            summary.as_deref(),
        )
        .await;
        {
            let mut st = self.state.lock().await;
            if st.status == ObserveStatus::Observing {
                st.status = ObserveStatus::Stopped;
            }
            st.latest_summary = summary;
        }
        tracing::info!(
            target: "screen_observe",
            observe_id = %self.observe_id,
            "screen observation ended"
        );
    }
}

// ─── manager (one observation at a time) ────────────────────────────────

struct ManagedObserve {
    session: Arc<ScreenObserveSession>,
    done: Arc<tokio::sync::Notify>,
}

fn active_observe() -> &'static Mutex<Option<ManagedObserve>> {
    static ACTIVE: OnceLock<Mutex<Option<ManagedObserve>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(None))
}

/// Start an observation. One at a time (single-operator desktop — mirrors
/// the clip recorder's one-in-flight rule); a second start while one runs
/// is an error the caller surfaces.
#[allow(clippy::too_many_arguments)]
pub async fn start_screen_observation(
    config: ObserveConfig,
    target: ObserveTarget,
    resources: Arc<AgentResources>,
    principal: String,
    workspace: String,
) -> Result<ObserveStatusView, String> {
    let mut slot = active_observe().lock().await;
    if let Some(existing) = slot.as_ref() {
        let view = existing.session.status_view().await;
        if view.status == ObserveStatus::Observing {
            return Err(format!(
                "an observation is already running ({}: {})",
                view.observe_id, view.purpose
            ));
        }
    }
    let observe_id = format!("observe-{}", uuid::Uuid::new_v4().simple());
    let narrator: Arc<dyn FrameNarrator> =
        Arc::new(RouterFrameNarrator::from_global_with_telemetry(
            resources.event_broadcaster.clone(),
            &principal,
            &workspace,
            &observe_id,
        )?);
    let summarizer: Arc<dyn Summarizer> =
        crate::media_rails::meeting::default_summarizer_with_telemetry(
            resources.event_broadcaster.clone(),
            Some((principal.clone(), workspace.clone())),
            Some(observe_id.clone()),
        );
    let announce = format!(
        "👁 observing — {} ({} mode{})",
        target.purpose,
        target.mode.as_str(),
        target
            .watch_for
            .as_deref()
            .map(|w| format!("; alert when: {w}"))
            .unwrap_or_default()
    );
    let sink: Arc<dyn TranscriptSink> = Arc::new(ChatThreadTranscriptSink::from_env(
        Some(config.thread.clone()),
        Some((principal.clone(), workspace.clone())),
        Some(config.session_title.clone()),
        Some(announce),
    ));
    let session = Arc::new(ScreenObserveSession::new(
        observe_id,
        config,
        target,
        narrator,
        summarizer,
        sink,
        resources,
        principal,
        workspace,
        FrameSource::Host,
    ));
    let done = Arc::new(tokio::sync::Notify::new());
    {
        let session = session.clone();
        let done = done.clone();
        tokio::spawn(async move {
            session.clone().run().await;
            done.notify_waiters();
        });
    }
    let view = session.status_view().await;
    *slot = Some(ManagedObserve { session, done });
    Ok(view)
}

/// Stop the active observation (if any) and wait (bounded) for its teardown
/// — the returned view carries the final summary.
pub async fn stop_screen_observation() -> Result<Option<ObserveStatusView>, String> {
    let managed = {
        let slot = active_observe().lock().await;
        match slot.as_ref() {
            Some(managed) => (managed.session.clone(), managed.done.clone()),
            None => return Ok(None),
        }
    };
    let (session, done) = managed;
    {
        let view = session.status_view().await;
        if view.status != ObserveStatus::Observing {
            return Ok(Some(view));
        }
    }
    session.cancel.cancel();
    // Teardown = final summary (local LLM) + memory write; bounded wait.
    let _ = tokio::time::timeout(Duration::from_secs(45), done.notified()).await;
    Ok(Some(session.status_view().await))
}

/// Status of the current (or most recent) observation.
pub async fn screen_observation_status() -> Option<ObserveStatusView> {
    let slot = active_observe().lock().await;
    match slot.as_ref() {
        Some(managed) => Some(managed.session.status_view().await),
        None => None,
    }
}

/// Retarget the RUNNING observation (notes ⇄ watch, new condition/purpose).
/// `Ok(None)` = nothing running.
pub async fn retarget_screen_observation(
    update: ObserveTargetUpdate,
) -> Result<Option<ObserveStatusView>, String> {
    let session = {
        let slot = active_observe().lock().await;
        match slot.as_ref() {
            Some(managed) => managed.session.clone(),
            None => return Ok(None),
        }
    };
    if session.status_view().await.status != ObserveStatus::Observing {
        return Ok(None);
    }
    session.retarget(update).await?;
    Ok(Some(session.status_view().await))
}

// ─── client-pushed frame observation (iOS broadcast) ──────────────────────

/// A registered pushed-frame ingest: the newest-frame slot the loop `take()`s,
/// plus the scope + upload token the ingest endpoint verifies.
struct RegisteredFrameIngest {
    latest: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    principal: String,
    workspace: String,
    upload_token: String,
}

fn pushed_frame_registry(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, RegisteredFrameIngest>> {
    static REG: OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, RegisteredFrameIngest>>,
    > = OnceLock::new();
    REG.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn register_pushed_frames(
    observe_id: &str,
    latest: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    principal: &str,
    workspace: &str,
    upload_token: &str,
) {
    pushed_frame_registry().lock().unwrap().insert(
        observe_id.to_string(),
        RegisteredFrameIngest {
            latest,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            upload_token: upload_token.to_string(),
        },
    );
}

fn deregister_pushed_frames(observe_id: &str) {
    pushed_frame_registry().lock().unwrap().remove(observe_id);
}

/// Result of a pushed-frame ingest — `Gone` is uniform for unknown session,
/// scope mismatch, and bad token (no existence oracle), mirroring the audio path.
pub enum FrameIngestResult {
    Accepted,
    Gone,
}

/// Ingest one client-pushed frame (JPEG/PNG bytes) for `observe_id`; the loop
/// picks it up on its next cadence tick (newest-wins).
pub fn push_observe_frame(
    observe_id: &str,
    principal: &str,
    workspace: &str,
    upload_token: &str,
    frame: Vec<u8>,
) -> FrameIngestResult {
    let reg = pushed_frame_registry().lock().unwrap();
    match reg.get(observe_id) {
        Some(entry)
            if entry.principal == principal
                && entry.workspace == workspace
                && entry.upload_token == upload_token =>
        {
            *entry.latest.lock().unwrap() = Some(frame);
            FrameIngestResult::Accepted
        },
        _ => FrameIngestResult::Gone,
    }
}

/// Start a screen observation whose frames are PUSHED by a remote client (the
/// iOS broadcast extension) instead of captured from the host. Narration lands
/// in `config.thread` — pass the meeting thread to weave screen notes into the
/// meeting transcript. Returns the status view plus the per-session frame upload
/// token the client echoes on `POST /screen/observe/frame`.
pub async fn start_client_screen_observation(
    config: ObserveConfig,
    target: ObserveTarget,
    resources: Arc<AgentResources>,
    principal: String,
    workspace: String,
) -> Result<(ObserveStatusView, String), String> {
    // Client observations live in their OWN multi-session registry, decoupled
    // from the single host slot — so a desktop host observation and any number of
    // mobile client observations run simultaneously (no cross-contention).
    let observe_id = format!("observe-{}", uuid::Uuid::new_v4().simple());
    let upload_token = format!("ft-{}", uuid::Uuid::new_v4().simple());
    let latest: Arc<std::sync::Mutex<Option<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(None));
    register_pushed_frames(
        &observe_id,
        latest.clone(),
        &principal,
        &workspace,
        &upload_token,
    );

    let narrator: Arc<dyn FrameNarrator> =
        Arc::new(RouterFrameNarrator::from_global_with_telemetry(
            resources.event_broadcaster.clone(),
            &principal,
            &workspace,
            &observe_id,
        )?);
    let summarizer: Arc<dyn Summarizer> =
        crate::media_rails::meeting::default_summarizer_with_telemetry(
            resources.event_broadcaster.clone(),
            Some((principal.clone(), workspace.clone())),
            Some(observe_id.clone()),
        );
    let announce = format!(
        "👁 observing — {} ({} mode{})",
        target.purpose,
        target.mode.as_str(),
        target
            .watch_for
            .as_deref()
            .map(|w| format!("; alert when: {w}"))
            .unwrap_or_default()
    );
    let sink: Arc<dyn TranscriptSink> = Arc::new(ChatThreadTranscriptSink::from_env(
        Some(config.thread.clone()),
        Some((principal.clone(), workspace.clone())),
        Some(config.session_title.clone()),
        Some(announce),
    ));
    let session = Arc::new(ScreenObserveSession::new(
        observe_id.clone(),
        config,
        target,
        narrator,
        summarizer,
        sink,
        resources,
        principal,
        workspace,
        FrameSource::Pushed(latest),
    ));
    let done = Arc::new(tokio::sync::Notify::new());
    {
        let session = session.clone();
        let done = done.clone();
        let observe_id = observe_id.clone();
        tokio::spawn(async move {
            session.clone().run().await;
            deregister_pushed_frames(&observe_id);
            client_observes().lock().await.remove(&observe_id);
            done.notify_waiters();
        });
    }
    let view = session.status_view().await;
    client_observes()
        .lock()
        .await
        .insert(observe_id, ManagedObserve { session, done });
    Ok((view, upload_token))
}

/// The client (mobile) observation registry — separate from the single host
/// slot so the two run concurrently.
fn client_observes() -> &'static Mutex<std::collections::HashMap<String, ManagedObserve>> {
    static REG: OnceLock<Mutex<std::collections::HashMap<String, ManagedObserve>>> =
        OnceLock::new();
    REG.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Stop a specific client observation (used when its broadcast/meeting ends).
/// Bounded wait for teardown; returns the final view, or `None` if not found.
pub async fn stop_client_screen_observation(observe_id: &str) -> Option<ObserveStatusView> {
    let managed = {
        let reg = client_observes().lock().await;
        reg.get(observe_id)
            .map(|m| (m.session.clone(), m.done.clone()))
    };
    let (session, done) = managed?;
    if session.status_view().await.status == ObserveStatus::Observing {
        session.cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(45), done.notified()).await;
    }
    Some(session.status_view().await)
}

#[cfg(test)]
mod narration_verdict_tests {
    use super::{parse_narration_verdict, NarrationKind};

    #[test]
    fn narration_verdict_accepts_only_the_declared_schema() {
        let (verdict, valid) =
            parse_narration_verdict(r#"{"verdict":"alert","line":"Payment failed"}"#);

        assert!(valid);
        assert_eq!(verdict.kind, NarrationKind::Alert);
        assert_eq!(verdict.line, "Payment failed");

        for malformed in [
            r#"{"verdict":"unexpected","line":"ignored"}"#,
            r#"{"verdict":"note"}"#,
            r#"{"verdict":"note","line":7}"#,
            "not-json",
        ] {
            let (_, valid) = parse_narration_verdict(malformed);
            assert!(!valid, "unexpectedly accepted: {malformed}");
        }
    }

    #[test]
    fn malformed_narration_keeps_the_existing_safe_fallback() {
        let (verdict, valid) = parse_narration_verdict("plain fallback note");

        assert!(!valid);
        assert_eq!(verdict.kind, NarrationKind::Note);
        assert_eq!(verdict.line, "plain fallback note");
    }
}
