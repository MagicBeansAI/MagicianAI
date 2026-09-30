//! Passive meeting listener — the SECOND meeting rail (plan:
//! `docs/archive/plans/2026-06-10-meetings-surface-passive-listener-plan.md`).
//!
//! Magician does NOT join the call. It listens locally — whole-display (system)
//! audio for the remote side + the user's microphone for the local side — and
//! streams a live transcript into the meeting's chat thread via the same
//! non-dispatching transcript endpoint the attendee rail uses, with a rolling
//! summary and a teardown memory write. No browser, no BlackHole, no responder:
//! nothing here can speak or trigger an agent turn by construction.
//!
//! The two audio tracks stay separate through STT and are labeled at the merge:
//! the mic track is `You`; the system track keeps the STT's diarized speaker
//! label, falling back to `Remote`. Raw audio is never persisted.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::magician_v2::media_seam::openai_streaming_stt::{pcm16_rms, DEFAULT_SILENCE_RMS};
use crate::magician_v2::media_seam::AudioStage;
use crate::magician_v2::media_seam::{
    AudioChunk, StreamAudioFormat, StreamingSttEvent, StreamingSttProvider, StreamingSttSession,
};

use crate::magician_v2::media_seam::audio::{AudioSource, CaptureTarget};
use crate::magician_v2::media_seam::meeting_memory::{
    MeetingMemoryWriter, NoopMeetingMemoryWriter,
};
use crate::magician_v2::media_seam::meeting_session_engine::TranscriptTurn;
use crate::magician_v2::media_seam::summarizer::Summarizer;
use crate::magician_v2::media_seam::transcript_sink::{NoopTranscriptSink, TranscriptSink};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassiveStatus {
    Listening,
    Stopping,
    Stopped,
    Failed,
}

/// Configuration for one passive listening session. The thread/title come from
/// the SHARED meeting resolver ([`super::resolve_meeting_thread`]) so a passive
/// session and an attendee join of the same meeting converge on the same
/// dated chat thread.
#[derive(Debug, Clone)]
pub struct PassiveListenerConfig {
    /// Per-meeting `ui_thread_id` the transcript streams into.
    pub thread: String,
    /// Display title for the thread's chat session.
    pub session_title: String,
    /// Human meeting title, when known.
    pub title: Option<String>,
    /// Meeting URL, when known (display metadata only — never opened).
    pub url: Option<String>,
    /// `YYYY-MM-DD` used in the thread id.
    pub date: String,
    /// Capture the user's microphone as the `You` track.
    pub capture_mic: bool,
    /// Re-summarize after this many new transcript turns.
    pub summarize_every_turns: usize,
    /// Optional canonical Meeting profile override for this session.
    pub audio_profile: Option<String>,
    /// Optional canonical per-stage selections for this session.
    pub audio_stage_options: BTreeMap<AudioStage, String>,
}

struct PassiveState {
    status: PassiveStatus,
    transcript: Vec<TranscriptTurn>,
    latest_summary: Option<String>,
    turns_since_summary: usize,
}

/// Flatten the transcript for the summarizer (chronological, speaker-prefixed).
fn transcript_text(turns: &[TranscriptTurn]) -> String {
    turns
        .iter()
        .map(|t| match &t.speaker {
            Some(s) => format!("{}: {}", s, t.text),
            None => t.text.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Normalize text for echo comparison: lowercase, alphanumerics only, single
/// spaces — so STT punctuation/casing differences between the two tracks don't
/// defeat the duplicate check.
fn normalize_for_echo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = true;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    out.trim_end().to_string()
}

/// Sustained system-track silence after which the listener stops itself
/// (meeting-over heuristic — the display goes quiet when the call ends).
/// `MEET_LISTEN_AUTO_STOP_SECS` overrides; `0` disables.
fn auto_stop_silence_limit() -> Option<Duration> {
    const DEFAULT_SECS: u64 = 300;
    let secs = std::env::var("MEET_LISTEN_AUTO_STOP_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Absolute listener lifetime backstop. `MEET_LISTEN_MAX_SECS` overrides;
/// `0` disables.
fn max_listen_duration() -> Option<Duration> {
    const DEFAULT_SECS: u64 = 4 * 3600;
    let secs = std::env::var("MEET_LISTEN_MAX_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Owns one passive listening session: two capture→STT tracks merged into one
/// transcript, streamed display-only into the meeting thread, summarized on a
/// rolling cadence, persisted to memory at teardown.
pub struct PassiveMeetingSession {
    config: PassiveListenerConfig,
    summarizer: Arc<dyn Summarizer>,
    stt: Arc<dyn StreamingSttProvider>,
    /// Optional dedicated provider for the mic ("You") track. The mic is a
    /// single-speaker source, so the default diarize provider's labels are
    /// wasted on it and its longer windows double the mic latency — the
    /// builder wires the plain (non-diarize) provider here.
    mic_stt: Option<Arc<dyn StreamingSttProvider>>,
    system_source: Arc<dyn AudioSource>,
    mic_source: Option<Arc<dyn AudioSource>>,
    transcript_sink: Arc<dyn TranscriptSink>,
    memory_writer: Arc<dyn MeetingMemoryWriter>,
    cancel: CancellationToken,
    /// Pause gate: while set, both track pumps DROP captured chunks before STT
    /// — nothing is transcribed, posted, or summarized until resume. The
    /// capture helpers keep running (cheap), so resume is instant.
    paused: Arc<AtomicBool>,
    /// Coalesces the off-loop rolling summaries: only one summarize task runs
    /// at a time, so a slow local LLM never stacks tasks (and never blocks the
    /// merge loop — see `spawn_resummarize`).
    summarize_inflight: Arc<AtomicBool>,
    /// Recent SYSTEM-track finals (normalized text + arrival time), used to
    /// suppress echo duplicates on the mic track: on speakers, remote voices
    /// leak into the microphone and would otherwise be re-recorded as `You`.
    recent_system_finals: Mutex<VecDeque<(Instant, String)>>,
    state: Arc<Mutex<PassiveState>>,
}

impl PassiveMeetingSession {
    pub fn new(
        config: PassiveListenerConfig,
        summarizer: Arc<dyn Summarizer>,
        stt: Arc<dyn StreamingSttProvider>,
        system_source: Arc<dyn AudioSource>,
        mic_source: Option<Arc<dyn AudioSource>>,
    ) -> Self {
        Self {
            config,
            summarizer,
            stt,
            mic_stt: None,
            system_source,
            mic_source,
            transcript_sink: Arc::new(NoopTranscriptSink),
            memory_writer: Arc::new(NoopMeetingMemoryWriter),
            cancel: CancellationToken::new(),
            paused: Arc::new(AtomicBool::new(false)),
            summarize_inflight: Arc::new(AtomicBool::new(false)),
            recent_system_finals: Mutex::new(VecDeque::new()),
            state: Arc::new(Mutex::new(PassiveState {
                status: PassiveStatus::Listening,
                transcript: Vec::new(),
                latest_summary: None,
                turns_since_summary: 0,
            })),
        }
    }

    pub fn with_transcript_sink(mut self, sink: Arc<dyn TranscriptSink>) -> Self {
        self.transcript_sink = sink;
        self
    }

    pub fn with_memory_writer(mut self, writer: Arc<dyn MeetingMemoryWriter>) -> Self {
        self.memory_writer = writer;
        self
    }

    /// Dedicated STT provider for the mic track (see the field doc).
    pub fn with_mic_stt(mut self, stt: Arc<dyn StreamingSttProvider>) -> Self {
        self.mic_stt = Some(stt);
        self
    }

    pub fn config(&self) -> &PassiveListenerConfig {
        &self.config
    }

    pub async fn status(&self) -> PassiveStatus {
        self.state.lock().await.status
    }

    pub async fn latest_summary(&self) -> Option<String> {
        self.state.lock().await.latest_summary.clone()
    }

    /// Mark capture non-live before firing cancellation. Tail STT, the final
    /// summary, and the memory write continue during `Stopping`, but callers
    /// must no longer treat the audio source as active.
    pub async fn stop(&self) -> PassiveStatus {
        let status = {
            let mut state = self.state.lock().await;
            if state.status == PassiveStatus::Listening {
                state.status = PassiveStatus::Stopping;
            }
            state.status
        };
        self.cancel.cancel();
        status
    }

    /// Pause/resume capture (see the `paused` field). Resume is instant — the
    /// capture helpers keep running while paused.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Spawn one capture→STT track: the source streams chunks until cancelled,
    /// the pump pushes them into its own STT session (finishing it when capture
    /// ends so the recognizer flushes). Each track has its OWN event channel so
    /// the merge loop knows which side a final came from.
    fn spawn_track(
        self: &Arc<Self>,
        source: Arc<dyn AudioSource>,
        stt_session: Box<dyn StreamingSttSession>,
        label: &'static str,
        raw_activity: Option<(mpsc::Sender<()>, f64)>,
    ) {
        let (chunk_tx, mut chunk_rx) = mpsc::channel::<AudioChunk>(64);
        {
            let cancel = self.cancel.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    result = source.run(chunk_tx) => {
                        if let Err(err) = result {
                            tracing::warn!(target: "meet_bot", track = label, error = %err, "passive audio source ended with error");
                        }
                    }
                }
            });
        }
        let paused = self.paused.clone();
        tokio::spawn(async move {
            while let Some(chunk) = chunk_rx.recv().await {
                // Paused: drop chunks BEFORE STT — nothing gets transcribed.
                if paused.load(Ordering::SeqCst) {
                    continue;
                }
                // Listener liveness follows the captured PCM, not STT output.
                // A provider can stall or reject a valid segment while people
                // are still speaking; tying auto-stop to transcript events
                // incorrectly ends the meeting in that case.
                if let Some((activity_tx, threshold)) = raw_activity.as_ref() {
                    if pcm16_rms(&chunk.pcm) >= *threshold {
                        let _ = activity_tx.try_send(());
                    }
                }
                if let Err(err) = stt_session.push_audio(chunk).await {
                    tracing::warn!(target: "meet_bot", track = label, error = %err, "passive stt push_audio failed");
                    break;
                }
            }
            let _ = stt_session.finish().await;
        });
    }

    /// Record a SYSTEM-track (remote) final: remember its normalized text for
    /// echo suppression, then record.
    async fn record_system_turn(&self, speaker: String, text: String) {
        {
            let normalized = normalize_for_echo(&text);
            let mut recent = self.recent_system_finals.lock().await;
            recent.push_back((Instant::now(), normalized));
            while recent.len() > 24 {
                recent.pop_front();
            }
        }
        self.record_turn(speaker, text).await;
    }

    /// Record a MIC ("You") final — unless it reads as acoustic echo of the
    /// system track. On speakers, remote voices leak into the microphone and
    /// would be re-recorded as `You` — and an APP-level mute (Meet's mute
    /// button) does not silence the OS input device, so during a "muted"
    /// meeting nearly every mic final is leaked remote speech.
    ///
    /// Detection is token-COVERAGE based, not exact-match: the STT of an
    /// acoustic re-capture (room → mic) rarely matches the display-audio STT
    /// verbatim, which is exactly how the previous substring check let a
    /// muted meeting flood the transcript with bogus `You:` turns. Coverage
    /// is measured against the union of recent system-final tokens, with a
    /// relaxed threshold when remote audio was playing moments ago.
    async fn record_mic_turn(&self, text: String) {
        const ECHO_WINDOW: Duration = Duration::from_secs(20);
        /// A system final this recent means remote audio was just playing —
        /// apply the relaxed (suspicious) threshold.
        const SYSTEM_ACTIVE_WINDOW: Duration = Duration::from_secs(4);
        /// Token coverage at/above which a mic final is echo (idle system).
        const COVERAGE_ECHO: f32 = 0.6;
        /// Relaxed threshold while the system track is actively speaking.
        const COVERAGE_ECHO_ACTIVE: f32 = 0.35;

        let normalized = normalize_for_echo(&text);
        if normalized.is_empty() {
            return;
        }
        let mic_tokens: Vec<&str> = normalized.split(' ').filter(|t| !t.is_empty()).collect();
        let is_echo = {
            let recent = self.recent_system_finals.lock().await;
            let mut system_tokens: std::collections::HashSet<&str> =
                std::collections::HashSet::new();
            let mut system_active = false;
            for (at, sys) in recent.iter() {
                if at.elapsed() > ECHO_WINDOW {
                    continue;
                }
                if at.elapsed() <= SYSTEM_ACTIVE_WINDOW {
                    system_active = true;
                }
                system_tokens.extend(sys.split(' ').filter(|t| !t.is_empty()));
            }
            let covered = mic_tokens
                .iter()
                .filter(|token| system_tokens.contains(**token))
                .count();
            let coverage = if mic_tokens.is_empty() {
                0.0
            } else {
                covered as f32 / mic_tokens.len() as f32
            };
            if mic_tokens.len() <= 2 {
                // Very short finals ("yes", "okay") are common GENUINE
                // cross-speaker repeats — drop only when every token just
                // came out of the speakers while remote audio was playing.
                system_active && coverage >= 1.0
            } else {
                coverage
                    >= if system_active {
                        COVERAGE_ECHO_ACTIVE
                    } else {
                        COVERAGE_ECHO
                    }
            }
        };
        if is_echo {
            tracing::debug!(
                target: "meet_bot",
                "passive mic: dropped echo of recent system audio"
            );
            return;
        }
        self.record_turn("You".to_string(), text).await;
    }

    /// Record one finalized turn: stream it to the meeting thread (display-only)
    /// + append to the in-memory transcript + rolling re-summary cadence (the
    /// summary runs OFF-loop so a slow local LLM never stalls live transcripts).
    async fn record_turn(&self, speaker: String, text: String) {
        let turn = TranscriptTurn {
            at_ms: 0,
            speaker: Some(speaker),
            text,
        };
        self.transcript_sink.post_turn(&turn).await;
        let should_summarize = {
            let mut st = self.state.lock().await;
            st.transcript.push(turn);
            st.turns_since_summary += 1;
            st.turns_since_summary >= self.config.summarize_every_turns
        };
        if should_summarize {
            self.spawn_resummarize();
        }
    }

    /// Rolling re-summary on its own task, coalesced: at most one in flight,
    /// and a summary that completes after teardown started (cancel fired) is
    /// discarded so it can't clobber the awaited FINAL summary with an older
    /// snapshot.
    fn spawn_resummarize(&self) {
        if self.cancel.is_cancelled() {
            return; // teardown owns the final summary
        }
        if self.summarize_inflight.swap(true, Ordering::SeqCst) {
            return; // one already running; the next threshold re-triggers
        }
        let summarizer = self.summarizer.clone();
        let state = self.state.clone();
        let inflight = self.summarize_inflight.clone();
        let cancel = self.cancel.clone();
        tokio::spawn(async move {
            let transcript = {
                let st = state.lock().await;
                transcript_text(&st.transcript)
            };
            if !transcript.trim().is_empty() {
                match summarizer.summarize(&transcript).await {
                    Ok(summary) => {
                        if !cancel.is_cancelled() {
                            let mut st = state.lock().await;
                            st.latest_summary = Some(summary);
                            st.turns_since_summary = 0;
                        }
                    },
                    Err(err) => {
                        tracing::warn!(target: "meet_bot", error = %err, "passive meeting summarization failed");
                    },
                }
            }
            inflight.store(false, Ordering::SeqCst);
        });
    }

    /// Awaited full re-summary — teardown only (the final summary must complete
    /// before the memory write).
    async fn resummarize(&self) {
        let transcript = {
            let st = self.state.lock().await;
            transcript_text(&st.transcript)
        };
        if transcript.trim().is_empty() {
            return;
        }
        match self.summarizer.summarize(&transcript).await {
            Ok(summary) => {
                let mut st = self.state.lock().await;
                st.latest_summary = Some(summary);
                st.turns_since_summary = 0;
            },
            Err(err) => {
                tracing::warn!(target: "meet_bot", error = %err, "passive meeting summarization failed");
            },
        }
    }

    async fn set_status(&self, status: PassiveStatus) {
        self.state.lock().await.status = status;
    }

    /// Run the listener until stopped (or the system capture ends). Spawned by
    /// the manager; teardown (final summary + memory write) runs before return.
    pub async fn start(self: Arc<Self>) {
        tracing::info!(
            target: "meet_bot",
            thread = %self.config.thread,
            mic = self.config.capture_mic,
            "PassiveMeetingSession.start (listening — not joining)"
        );

        // Stop can arrive after registration but before this task is polled.
        // Avoid opening an STT stream for an already-cancelled capture.
        if self.cancel.is_cancelled() {
            self.set_status(PassiveStatus::Stopped).await;
            return;
        }

        // System (remote) track — whole-display audio: the only capture that
        // reliably hears every call surface, and there is no own-voice playback
        // to gate against (the passive rail never speaks).
        let (sys_tx, mut sys_rx) = mpsc::channel::<StreamingSttEvent>(64);
        let sys_stt = match self
            .stt
            .open_session(StreamAudioFormat::default(), sys_tx)
            .await
        {
            Ok(s) => s,
            Err(err) => {
                tracing::error!(target: "meet_bot", error = %err, "passive listener: system STT open failed");
                self.set_status(PassiveStatus::Failed).await;
                return;
            },
        };
        self.system_source
            .set_capture_target(CaptureTarget::DisplayAudio)
            .await;
        let (system_activity_tx, mut system_activity_rx) = mpsc::channel::<()>(1);
        self.spawn_track(
            self.system_source.clone(),
            sys_stt,
            "system",
            // Keep listener liveness independent from the STT gate override:
            // `MEET_STT_SILENCE_RMS=0` may disable transcription filtering,
            // but zero-filled PCM must still allow meeting auto-stop.
            Some((system_activity_tx, DEFAULT_SILENCE_RMS)),
        );

        // Mic ("You") track — optional, best-effort: a missing mic or denied
        // permission degrades to system-only listening rather than failing.
        // Uses the dedicated plain provider when wired (single speaker —
        // diarization is wasted and its longer windows double mic latency).
        let (mic_tx, mut mic_rx) = mpsc::channel::<StreamingSttEvent>(64);
        let mut mic_open = false;
        if self.config.capture_mic {
            if let Some(mic_source) = self.mic_source.clone() {
                let mic_provider = self.mic_stt.clone().unwrap_or_else(|| self.stt.clone());
                match mic_provider
                    .open_session(StreamAudioFormat::default(), mic_tx)
                    .await
                {
                    Ok(mic_stt) => {
                        self.spawn_track(mic_source, mic_stt, "mic", None);
                        mic_open = true;
                    },
                    Err(err) => {
                        tracing::warn!(target: "meet_bot", error = %err, "passive listener: mic STT open failed; continuing system-only");
                    },
                }
            }
        }

        // Merge loop: label and record finals from both tracks.
        //
        // Auto-stop: the listener has no signal for "the user left the call"
        // (capture is whole-display), so SUSTAINED raw-audio silence on the
        // system track is the meeting-over heuristic. This deliberately does
        // not use STT events: provider stalls/rejections must not end an
        // ongoing meeting. Mic activity does not count because the room can
        // remain noisy after a call ends. A max-duration cap backstops the
        // heuristic. Both limits are env-tunable; paused time never counts as
        // silence.
        let started_at = Instant::now();
        let mut system_track_ended = false;
        let mut system_activity_open = true;
        let mut last_system_audio = Instant::now();
        let mut auto_stop_note: Option<String> = None;
        let silence_limit = auto_stop_silence_limit();
        let duration_limit = max_listen_duration();
        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => break,
                activity = system_activity_rx.recv(), if system_activity_open => {
                    match activity {
                        Some(()) => last_system_audio = Instant::now(),
                        None => system_activity_open = false,
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(15)) => {
                    if self.is_paused() {
                        // Treat paused time as activity — resuming after a
                        // long pause must not instantly trip the silence stop.
                        last_system_audio = Instant::now();
                    } else if let Some(limit) = silence_limit {
                        if last_system_audio.elapsed() >= limit {
                            auto_stop_note = Some(format!(
                                "Listening stopped automatically after {} minutes without meeting audio.",
                                limit.as_secs() / 60
                            ));
                            break;
                        }
                    }
                    if let Some(limit) = duration_limit {
                        if started_at.elapsed() >= limit {
                            auto_stop_note = Some(format!(
                                "Listening stopped automatically at the {}-hour limit.",
                                limit.as_secs() / 3600
                            ));
                            break;
                        }
                    }
                }
                ev = sys_rx.recv() => match ev {
                    Some(StreamingSttEvent::Final { text, speaker, .. }) => {
                        let text = text.trim().to_string();
                        if !text.is_empty() {
                            let speaker = speaker
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or_else(|| "Remote".to_string());
                            tracing::info!(target: "meet_bot", "heard (passive/{speaker}): {text}");
                            self.record_system_turn(speaker, text).await;
                        }
                    }
                    Some(StreamingSttEvent::Error { reason }) => {
                        tracing::warn!(target: "meet_bot", reason = %reason, "passive system STT error event");
                    }
                    Some(_) => {
                        // Transcript progress is handled above; liveness comes
                        // from raw PCM through `system_activity_rx`.
                    }
                    None => {
                        // System capture ended → listener over.
                        system_track_ended = true;
                        break;
                    }
                },
                ev = mic_rx.recv(), if mic_open => match ev {
                    Some(StreamingSttEvent::Final { text, .. }) => {
                        let text = text.trim().to_string();
                        if !text.is_empty() {
                            tracing::info!(target: "meet_bot", "heard (passive/You): {text}");
                            self.record_mic_turn(text).await;
                        }
                    }
                    Some(StreamingSttEvent::Error { reason }) => {
                        tracing::warn!(target: "meet_bot", reason = %reason, "passive mic STT error event");
                    }
                    Some(_) => {}
                    None => mic_open = false, // mic ended; keep listening to system
                },
            }
        }

        // Fence stragglers (idempotent): in-flight rolling summaries must not
        // clobber the FINAL summary computed below.
        self.cancel.cancel();

        // Drain the flushed tail: stopping capture triggers an STT flush whose
        // finals land AFTER the loop exits (the flush itself is a network
        // call). Without this bounded drain the meeting's closing utterances
        // would be missing from the transcript, summary, and memory write.
        // Both tracks drain CONCURRENTLY under one deadline — a slow system
        // flush must not starve the mic track of its window (the user's own
        // closing words are usually the LAST finals to arrive), and one shared
        // bound keeps total teardown comfortably inside the manager's stop wait.
        let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        let mut sys_draining = true;
        let mut mic_draining = mic_open;
        while sys_draining || mic_draining {
            tokio::select! {
                _ = tokio::time::sleep_until(drain_deadline) => break,
                ev = sys_rx.recv(), if sys_draining => match ev {
                    Some(StreamingSttEvent::Final { text, speaker, .. }) => {
                        let text = text.trim().to_string();
                        if !text.is_empty() {
                            let speaker = speaker
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or_else(|| "Remote".to_string());
                            self.record_system_turn(speaker, text).await;
                        }
                    }
                    Some(_) => {}
                    None => sys_draining = false,
                },
                ev = mic_rx.recv(), if mic_draining => match ev {
                    Some(StreamingSttEvent::Final { text, .. }) => {
                        let text = text.trim().to_string();
                        if !text.is_empty() {
                            self.record_mic_turn(text).await;
                        }
                    }
                    Some(_) => {}
                    None => mic_draining = false,
                },
            }
        }

        // A system track that died almost immediately with nothing heard is a
        // capture FAILURE (missing Screen Recording grant, helper missing) —
        // not a clean stop. Surface it as Failed so the API/UI don't show a
        // silent success.
        let transcript_empty = self.state.lock().await.transcript.is_empty();
        if system_track_ended && transcript_empty && started_at.elapsed() < Duration::from_secs(15)
        {
            tracing::error!(
                target: "meet_bot",
                thread = %self.config.thread,
                "passive listener: system capture ended immediately with no audio — \
                 check the Screen Recording permission / capture helper"
            );
            self.set_status(PassiveStatus::Failed).await;
            return;
        }

        // An auto-stop must be visible in the thread, not just the logs —
        // the user finds out WHY the listener ended where the transcript is.
        if let Some(note) = auto_stop_note.as_deref() {
            tracing::info!(target: "meet_bot", thread = %self.config.thread, "{note}");
            self.transcript_sink
                .post_turn(&TranscriptTurn {
                    at_ms: 0,
                    speaker: Some("Listener".to_string()),
                    text: note.to_string(),
                })
                .await;
        }

        // Teardown: final summary + memory write, then Stopped.
        self.resummarize().await;
        if let Some(summary) = self.latest_summary().await {
            // The thread is the durable record — post the final summary INTO
            // it. `latest_summary` otherwise lives only in this process's
            // session registry and vanishes when the ended session is reaped
            // (the "summary was visible, then gone" bug).
            self.transcript_sink
                .post_turn(&TranscriptTurn {
                    at_ms: 0,
                    speaker: Some("Meeting summary".to_string()),
                    text: summary.clone(),
                })
                .await;
            let fields = super::meeting_memory::meeting_summary_to_fields(&summary);
            let meta = super::meeting_memory::MeetingMemoryMeta {
                thread_id: Some(self.config.thread.clone()),
                mode: "passive",
                title: self.config.title.clone(),
                url: self.config.url.clone(),
                date: Some(self.config.date.clone()),
            };
            self.memory_writer.write_summary(fields, &meta).await;
        }
        self.set_status(PassiveStatus::Stopped).await;
        tracing::info!(target: "meet_bot", thread = %self.config.thread, "passive listener stopped");
    }
}

/// How long an ended passive session stays queryable before it's reaped. Public
/// for the same reason as the attendee rail's constant, and separately named so
/// neither glob re-export shadows the other.
pub const PASSIVE_ENDED_RETAIN_SECS: u64 = 300;

struct ManagedPassive {
    session: Arc<PassiveMeetingSession>,
    /// When the listener was registered. A monotonic `Instant`, not a wall
    /// clock: capture duration must not jump when the host clock is adjusted,
    /// and the registry is process-local so an `Instant` never has to outlive
    /// it.
    started_at: Instant,
    /// The scope that started this listener, when one was supplied. See
    /// [`super::meeting_manager::MeetingSummaryRow::scope`] — the registry is
    /// keyed by session id alone, so a scope-checked caller needs this to
    /// refuse another scope's session.
    scope: Option<(String, String)>,
    ended_at: Option<Instant>,
    done: Arc<tokio::sync::Notify>,
}

#[derive(Debug, Clone)]
pub struct PassiveStatusView {
    pub session_id: String,
    pub status: PassiveStatus,
    pub thread: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub capture_mic: bool,
    pub paused: bool,
    pub latest_summary: Option<String>,
    /// Whole seconds since the listener was registered — the capture duration a
    /// surface shows. Derived from a monotonic clock at read time.
    pub started_seconds_ago: u64,
    /// Whole seconds since teardown returned, or `None` while the listener is
    /// still running. An ended listener stays queryable for
    /// [`PASSIVE_ENDED_RETAIN_SECS`] and then disappears, so a surface that renders
    /// session state owes the operator this age rather than a bare status.
    pub ended_seconds_ago: Option<u64>,
    /// `(principal, workspace)` the capture was started under, when known.
    pub scope: Option<(String, String)>,
}

/// Process-global registry for live passive listeners (sibling of
/// [`super::MeetingSessionManager`] — kept separate so neither rail's lifecycle
/// leaks into the other).
#[derive(Default)]
pub struct PassiveMeetingSessionManager {
    sessions: Mutex<HashMap<String, ManagedPassive>>,
}

static PASSIVE_MANAGER: Lazy<Arc<PassiveMeetingSessionManager>> =
    Lazy::new(|| Arc::new(PassiveMeetingSessionManager::new()));

pub fn passive_meeting_manager() -> Arc<PassiveMeetingSessionManager> {
    PASSIVE_MANAGER.clone()
}

impl PassiveMeetingSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Register + spawn a prebuilt session; returns its `session_id`. `marker`
    /// enables the crash marker: written here, removed when `start()` returns —
    /// a marker that survives the process is an interrupted capture for the
    /// boot sweep.
    pub async fn spawn(
        self: &Arc<Self>,
        session: Arc<PassiveMeetingSession>,
        marker: Option<super::MarkerContext>,
    ) -> String {
        let session_id = format!("listen-{}", Uuid::new_v4());
        let marker_path = {
            let cfg = session.config();
            crate::magician_v2::media_seam::meeting_markers::write_marker_for_session(
                marker.as_ref(),
                &session_id,
                "passive",
                Some(&cfg.thread),
                cfg.title.as_deref(),
            )
        };
        let done = Arc::new(tokio::sync::Notify::new());
        {
            let mut map = self.sessions.lock().await;
            map.insert(
                session_id.clone(),
                ManagedPassive {
                    session: session.clone(),
                    started_at: Instant::now(),
                    scope: marker
                        .as_ref()
                        .map(|marker| (marker.principal.clone(), marker.workspace.clone())),
                    ended_at: None,
                    done: done.clone(),
                },
            );
        }
        let mgr = Arc::clone(self);
        let id_for_task = session_id.clone();
        tokio::spawn(async move {
            session.start().await;
            // Revoke client-push ingest for this session on EVERY teardown path
            // (stop, silence auto-stop, max-duration, idle, failure). No-op for
            // host sessions; for client sessions a subsequent POST then gets Gone.
            crate::magician_v2::media_seam::meeting_pushed::on_session_ended(&id_for_task);
            // Clean teardown ran (final summary + memory write) — clear the marker.
            if let Some(path) = marker_path {
                crate::magician_v2::media_seam::meeting_markers::remove_marker(&path);
            }
            {
                let mut map = mgr.sessions.lock().await;
                if let Some(m) = map.get_mut(&id_for_task) {
                    m.ended_at = Some(Instant::now());
                }
            }
            done.notify_one();
        });
        session_id
    }

    pub async fn status(&self, session_id: &str) -> Option<PassiveStatusView> {
        self.reap().await;
        let map = self.sessions.lock().await;
        let m = map.get(session_id)?;
        Some(Self::view(session_id, m).await)
    }

    pub async fn list(&self) -> Vec<PassiveStatusView> {
        self.reap().await;
        let map = self.sessions.lock().await;
        let mut rows = Vec::with_capacity(map.len());
        for (id, m) in map.iter() {
            rows.push(Self::view(id, m).await);
        }
        rows
    }

    async fn view(session_id: &str, m: &ManagedPassive) -> PassiveStatusView {
        let cfg = m.session.config();
        PassiveStatusView {
            session_id: session_id.to_string(),
            status: m.session.status().await,
            thread: cfg.thread.clone(),
            title: cfg.title.clone(),
            url: cfg.url.clone(),
            capture_mic: cfg.capture_mic,
            paused: m.session.is_paused(),
            latest_summary: m.session.latest_summary().await,
            started_seconds_ago: m.started_at.elapsed().as_secs(),
            ended_seconds_ago: m.ended_at.map(|at| at.elapsed().as_secs()),
            scope: m.scope.clone(),
        }
    }

    /// Pause/resume a live listener's capture. Errors on unknown/non-live ids.
    pub async fn set_paused(&self, session_id: &str, paused: bool) -> Result<(), String> {
        let session = {
            let map = self.sessions.lock().await;
            match map.get(session_id) {
                Some(m) => m.session.clone(),
                None => return Err(format!("unknown passive session_id: {session_id}")),
            }
        };
        if session.status().await != PassiveStatus::Listening {
            return Err(format!("passive session is not live: {session_id}"));
        }
        session.set_paused(paused);
        Ok(())
    }

    /// The live (not-yet-ended) listener already targeting `thread`, if any —
    /// the double-listen guard: two listeners on one meeting double-post every
    /// transcript line and double the STT spend.
    pub async fn find_live_by_thread(&self, thread: &str) -> Option<String> {
        let candidates = {
            let map = self.sessions.lock().await;
            map.iter()
                .filter(|(_, m)| m.ended_at.is_none() && m.session.config().thread == thread)
                .map(|(id, m)| (id.clone(), m.session.clone()))
                .collect::<Vec<_>>()
        };
        for (id, session) in candidates {
            if session.status().await == PassiveStatus::Listening {
                return Some(id);
            }
        }
        None
    }

    /// Request an immediate capture stop without waiting for teardown work.
    /// Client-pushed ingest is revoked synchronously so a phone receives the
    /// terminal 410 on its next upload while tail STT/finalization continues.
    pub async fn request_stop(
        &self,
        session_id: &str,
    ) -> Result<(PassiveStatus, Option<String>), String> {
        let session = {
            let map = self.sessions.lock().await;
            match map.get(session_id) {
                Some(m) => m.session.clone(),
                None => return Err(format!("unknown passive session_id: {session_id}")),
            }
        };
        let status = session.stop().await;
        crate::magician_v2::media_seam::meeting_pushed::on_session_ended(session_id);
        Ok((status, session.latest_summary().await))
    }

    /// Stop a listener and return its POST-teardown final summary (bounded wait,
    /// mirroring the attendee manager's `leave`; the window covers the tail
    /// drain + final summarize so callers usually get the FINAL summary).
    pub async fn stop(&self, session_id: &str) -> Result<Option<String>, String> {
        let (done, already_ended) = {
            let map = self.sessions.lock().await;
            match map.get(session_id) {
                Some(m) => (m.done.clone(), m.ended_at.is_some()),
                None => return Err(format!("unknown passive session_id: {session_id}")),
            }
        };
        let (status, _) = self.request_stop(session_id).await?;
        if !already_ended && status == PassiveStatus::Stopping {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(20), done.notified()).await;
        }
        Ok(self
            .status(session_id)
            .await
            .and_then(|view| view.latest_summary))
    }

    async fn reap(&self) {
        let mut map = self.sessions.lock().await;
        map.retain(|_, m| match m.ended_at {
            Some(t) => t.elapsed().as_secs() < PASSIVE_ENDED_RETAIN_SECS,
            None => true,
        });
    }
}

/// Build + start the real passive listener (display audio + optional mic +
/// chat-thread sink + scoped memory writer) and register it. macOS captures
/// with ScreenCaptureKit. Linux records the Pulse default-sink monitor.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub async fn start_passive_listener(
    config: PassiveListenerConfig,
    memory_writer: Arc<dyn MeetingMemoryWriter>,
    scope: Option<(String, String)>,
    marker: Option<super::MarkerContext>,
    broadcaster: Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    use crate::magician_v2::media_seam::meeting_bridge_linux::PulseAudioSource;
    #[cfg(target_os = "macos")]
    use crate::magician_v2::media_seam::meeting_bridge_macos::{
        MicrophoneAudioSource, ScreenCaptureAudioSource,
    };
    use crate::magician_v2::media_seam::summarizer::default_summarizer_with_telemetry;
    use crate::magician_v2::media_seam::transcript_sink::ChatThreadTranscriptSink;

    // Idempotent per meeting: a live listener already on this thread is
    // returned as-is instead of starting a duplicate capture pipeline.
    if let Some(existing) = passive_meeting_manager()
        .find_live_by_thread(&config.thread)
        .await
    {
        tracing::info!(
            target: "meet_bot",
            thread = %config.thread,
            session_id = %existing,
            "passive listener already live for this meeting; reusing it"
        );
        return Ok(existing);
    }

    let summarizer: Arc<dyn Summarizer> = default_summarizer_with_telemetry(
        broadcaster,
        scope.clone(),
        Some(format!("meeting:{}", config.thread)),
    );
    let (stt, _) = crate::magician_v2::media_seam::resolve_installed_surface_audio_pipeline(
        crate::magician_v2::media_seam::AudioSurface::Meeting,
        scope.clone(),
        config.audio_profile.as_deref(),
        &config.audio_stage_options,
        true,
    )
    .await?;
    let (mic_stt, _) = crate::magician_v2::media_seam::resolve_installed_surface_audio_pipeline(
        crate::magician_v2::media_seam::AudioSurface::Meeting,
        scope.clone(),
        config.audio_profile.as_deref(),
        &config.audio_stage_options,
        false,
    )
    .await?;
    #[cfg(target_os = "macos")]
    let system_source: Arc<dyn AudioSource> = Arc::new(ScreenCaptureAudioSource::new());
    #[cfg(target_os = "linux")]
    let system_source: Arc<dyn AudioSource> = Arc::new(PulseAudioSource::new());
    #[cfg(target_os = "macos")]
    let mic_source: Option<Arc<dyn AudioSource>> = if config.capture_mic {
        Some(Arc::new(MicrophoneAudioSource::new()))
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let mic_source: Option<Arc<dyn AudioSource>> = if config.capture_mic {
        Some(Arc::new(PulseAudioSource::microphone()))
    } else {
        None
    };
    let announce = match &config.url {
        Some(url) => format!("Listening to this meeting (passive, not joined): {url}"),
        None => "Listening to this meeting (passive, not joined).".to_string(),
    };
    let sink: Arc<dyn TranscriptSink> = Arc::new(ChatThreadTranscriptSink::from_env(
        Some(config.thread.clone()),
        scope,
        Some(config.session_title.clone()),
        Some(announce),
    ));
    let session = Arc::new(
        PassiveMeetingSession::new(config, summarizer, stt, system_source, mic_source)
            .with_mic_stt(mic_stt)
            .with_transcript_sink(sink)
            .with_memory_writer(memory_writer),
    );
    Ok(passive_meeting_manager().spawn(session, marker).await)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::*;
    use async_trait::async_trait;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    // This module globs `media_seam::*` rather than `super::*`, so it does not
    // inherit the file's own imports.
    use std::time::Instant;

    use super::ManagedPassive;
    use crate::magician_v2::media_seam::audio::NoopAudioSource;
    use crate::magician_v2::media_seam::summarizer::SummarizerError;
    use crate::magician_v2::media_seam::NoopStreamingSttProvider;

    struct StubSummarizer;

    #[async_trait]
    impl Summarizer for StubSummarizer {
        async fn summarize(&self, _transcript: &str) -> Result<String, SummarizerError> {
            Ok("summary".to_string())
        }
    }

    fn test_session(thread: &str) -> Arc<PassiveMeetingSession> {
        Arc::new(PassiveMeetingSession::new(
            PassiveListenerConfig {
                thread: thread.to_string(),
                session_title: "Test meeting".to_string(),
                title: Some("Test meeting".to_string()),
                url: None,
                date: "2026-07-16".to_string(),
                capture_mic: false,
                summarize_every_turns: 8,
                audio_profile: None,
                audio_stage_options: BTreeMap::new(),
            },
            Arc::new(StubSummarizer),
            Arc::new(NoopStreamingSttProvider),
            Arc::new(NoopAudioSource),
            None,
        ))
    }

    #[tokio::test]
    async fn request_stop_marks_capture_non_live_before_teardown() {
        let manager = PassiveMeetingSessionManager::new();
        let session = test_session("meeting-stop-test");
        manager.sessions.lock().await.insert(
            "listen-test".to_string(),
            ManagedPassive {
                session: session.clone(),
                started_at: Instant::now(),
                scope: None,
                ended_at: None,
                done: Arc::new(tokio::sync::Notify::new()),
            },
        );

        assert_eq!(
            manager.find_live_by_thread("meeting-stop-test").await,
            Some("listen-test".to_string())
        );
        let (status, summary) = manager
            .request_stop("listen-test")
            .await
            .expect("stop request accepted");
        assert_eq!(status, PassiveStatus::Stopping);
        assert_eq!(summary, None);
        assert_eq!(session.status().await, PassiveStatus::Stopping);
        assert_eq!(manager.find_live_by_thread("meeting-stop-test").await, None);

        // A stop that races ahead of the spawned task must not open capture.
        session.clone().start().await;
        assert_eq!(session.status().await, PassiveStatus::Stopped);
    }
}

/// Other hosts have no meeting capture stack.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn start_passive_listener(
    _config: PassiveListenerConfig,
    _memory_writer: Arc<dyn MeetingMemoryWriter>,
    _scope: Option<(String, String)>,
    _marker: Option<super::MarkerContext>,
    _broadcaster: Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
) -> Result<String, String> {
    Err("the passive meeting listener needs macOS or Linux".into())
}
