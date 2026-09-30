//! Per-meeting orchestration skeleton for the Google-Meet participant bot.
//! See docs/plans/2026-06-06-gmeet-participant-bot-macos.md.
//!
//! This is a SKELETON: it models the session state + lifecycle and owns the
//! transcript buffer + periodic summarizer (already validated against Gemma 4
//! 12B). The audio bridge (BlackHole ↔ realtime channel), the streaming-STT
//! wiring, the wake-word gate, and the OpenAI-Realtime responder are TODO hooks
//! pending the macOS audio bridge (Spike 1+). Nothing here joins a meeting or
//! touches audio yet.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::magician_v2::media_seam::audio::{
    AudioSink, AudioSource, CaptureTarget, NoopAudioSink, NoopAudioSource,
};
use crate::magician_v2::media_seam::meeting_llm_tts_responder::LlmTtsResponder;
use crate::magician_v2::media_seam::meeting_magician_agent_responder::MagicianAgentResponder;
use crate::magician_v2::media_seam::meeting_memory::{
    MeetingMemoryWriter, NoopMeetingMemoryWriter,
};
use crate::magician_v2::media_seam::meeting_orchestrator_voice_responder::OrchestratorVoiceResponder;
use crate::magician_v2::media_seam::meeting_realtime_responder::RealtimeResponder;
use crate::magician_v2::media_seam::meeting_session::{derive_meeting_thread_id, MeetingConfig};
use crate::magician_v2::media_seam::responder::{MeetingResponder, NoopResponder, UtteranceAudio};
use crate::magician_v2::media_seam::summarizer::Summarizer;
use crate::magician_v2::media_seam::transcript_sink::{NoopTranscriptSink, TranscriptSink};
use crate::magician_v2::media_seam::{
    AudioChunk, NoopStreamingSttProvider, StreamAudioFormat, StreamingSttEvent,
    StreamingSttProvider, TtsProvider, TtsRequest,
};
use crate::magician_v2::media_seam::{BrowserJoin, NoopBrowserJoin};

/// True when the on-device macOS Speech helper binary is present (the same
/// binary the capture sources use). Used when registering the macOS provider.
pub fn macos_speech_helper_present() -> bool {
    let path = std::env::var("MAGICIAN_MACOS_MEET_AUDIO_BIN")
        .unwrap_or_else(|_| "./magician-macos-meet-audio.bin".to_string());
    std::path::Path::new(&path).exists()
}

/// Build the meeting bot's default responder (the "respond when addressed" seam).
///
/// Defaults to [`OrchestratorVoiceResponder`] — the agent's **realtime voice**
/// via the server `VoiceOrchestrator` (Presto with its tools + context, on
/// `gpt-realtime-2`, with server-side session rotation + context compaction). The
/// launcher wires this via [`MeetingSession::with_responder`].
///
/// Override via `MEET_BOT_RESPONDER`:
///   - unset / `orchestrator` → [`OrchestratorVoiceResponder`]
///   - `agent-tts` → [`MagicianAgentResponder`] (agent over REST → OpenAI TTS)
///   - `llm-tts` → [`LlmTtsResponder`] (local Ollama answer → OpenAI TTS)
///   - `realtime-direct` → [`RealtimeResponder`] (bot-local realtime, no agent tools)
///   - `noop` / `off` → [`NoopResponder`] (cue-only / tests)
///
/// `openai_api_key` is used by the TTS / direct-realtime fallbacks; the default
/// orchestrator path routes through the server and ignores it. Async because the
/// `realtime-direct` fallback opens its upstream session eagerly.
///
/// `thread` is the per-meeting chat thread the chat-posting responders write to
/// (see [`derive_meeting_thread_id`]). `None` leaves each responder on its own
/// env/default thread (`MEET_BOT_THREAD`, else `meeting-bot`). `scope` is the
/// invoking agent's `(principal, workspace)` so the responder lanes post into
/// that agent's chat store, not the env default (explicit `MEET_BOT_PRINCIPAL`/
/// `MEET_BOT_WORKSPACE` pins still win — see `with_scope`).
pub async fn default_meeting_responder(
    openai_api_key: impl Into<String>,
    thread: Option<String>,
    scope: Option<(String, String)>,
    broadcaster: Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
) -> Arc<dyn MeetingResponder> {
    let key = openai_api_key.into();
    let mode = std::env::var("MEET_BOT_RESPONDER").unwrap_or_default();
    match mode.trim().to_ascii_lowercase().as_str() {
        "agent-tts" | "agent_tts" => Arc::new(
            MagicianAgentResponder::from_env(key)
                .with_thread(thread)
                .with_scope(scope),
        ),
        "llm-tts" | "llm_tts" => Arc::new(LlmTtsResponder::from_env(key).with_telemetry(
            broadcaster,
            scope,
            thread.map(|thread| format!("meeting:{thread}")),
        )),
        "realtime-direct" | "realtime_direct" => {
            // Presto meeting joining runs on the FLAGSHIP gpt-realtime-2.1 (not the
            // cheaper default mini): meetings need accurate alphanumerics (names,
            // numbers, codes), reliable interruption handling, and stronger
            // instruction following. Override with MEET_BOT_REALTIME_MODEL.
            let meeting_model = std::env::var("MEET_BOT_REALTIME_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "gpt-realtime-2.1".to_string());
            match RealtimeResponder::connect(
                key,
                Some(meeting_model),
                None,
                broadcaster,
                scope.clone(),
            )
            .await
            {
                Ok(responder) => Arc::new(responder),
                Err(error) => {
                    tracing::warn!(
                        target: "meet_bot",
                        "MEET_BOT_RESPONDER=realtime-direct failed to connect ({error}); \
                         falling back to the orchestrator voice responder"
                    );
                    Arc::new(
                        OrchestratorVoiceResponder::from_env()
                            .with_thread(thread)
                            .with_scope(scope),
                    )
                },
            }
        },
        "noop" | "off" | "none" => Arc::new(NoopResponder),
        // Default (unset or "orchestrator"): the agent's realtime voice.
        _ => Arc::new(
            OrchestratorVoiceResponder::from_env()
                .with_thread(thread)
                .with_scope(scope),
        ),
    }
}

/// How much recent capture audio to keep so the just-spoken question can be
/// replayed to a realtime responder (we only learn an utterance was a wake
/// command after it has finished — see the settle gate).
const AUDIO_RING_RETAIN: Duration = Duration::from_secs(15);
/// Lead captured before the first transcript partial, to catch the speech onset
/// (the partial lands slightly after the user starts talking).
const UTTERANCE_LEAD: Duration = Duration::from_millis(600);
/// Fallback window when the utterance start wasn't observed (e.g. an engine that
/// emits a `Final` with no preceding partial).
const UTTERANCE_FALLBACK_WINDOW: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingStatus {
    Idle,
    Joining,
    Listening,
    Responding,
    Left,
    Failed,
}

/// One finalized transcript turn (optionally speaker-attributed).
#[derive(Debug, Clone)]
pub struct TranscriptTurn {
    pub at_ms: u64,
    pub speaker: Option<String>,
    pub text: String,
}

impl Default for MeetingConfig {
    fn default() -> Self {
        Self {
            meet_url: String::new(),
            display_name: "Magican".to_string(),
            wake_phrases: vec![
                "hey magican".to_string(),
                "hey magical".to_string(),
                "hey magician".to_string(),
                "magican".to_string(),
            ],
            summarize_every_turns: 40,
            responder_tail_turns: 20,
            title: None,
            meeting_date: None,
            audio_profile: None,
            audio_stage_options: BTreeMap::new(),
        }
    }
}

/// Lowercase, keep `[a-z0-9]`, collapse any other run to a single `-`, trim
/// dashes, and cap length so the thread id stays tidy.
pub(crate) fn slugify_thread_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let capped: String = out.trim_matches('-').chars().take(40).collect();
    capped.trim_matches('-').to_string()
}

/// Parse the Meet code from a `meet.google.com/<code>` URL (the first path
/// segment after the host), slugified. `meet.google.com/lookup/<alias>` links
/// carry the meeting id in the SECOND segment — taking "lookup" itself would
/// collide every such link into one thread. `None` if there's no usable segment.
pub(crate) fn meet_code_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split("://").last().unwrap_or(url);
    let path = after_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let mut segs = path.split('/').skip(1).filter(|s| !s.is_empty());
    let mut seg = segs.next()?;
    if seg.eq_ignore_ascii_case("lookup") {
        seg = segs.next()?;
    }
    let slug = slugify_thread_label(seg);
    (!slug.is_empty()).then_some(slug)
}

/// Stable short hex hash of the URL, for the rare case we can't parse a code.
/// blake3, NOT `DefaultHasher` — thread ids must survive restarts AND toolchain
/// upgrades, and `DefaultHasher`'s algorithm is explicitly unspecified across
/// Rust releases.
pub(crate) fn short_url_hash(url: &str) -> String {
    blake3::hash(url.as_bytes()).to_hex()[..8].to_string()
}

/// One resolved meeting identity, shared by BOTH rails (agent attendee and
/// passive listener) so a meeting converges on the same dated thread no matter
/// which mode captured it.
#[derive(Debug, Clone)]
pub struct ResolvedMeetingThread {
    /// The per-meeting `ui_thread_id` (`meeting-<label>-<date>`).
    pub thread: String,
    /// Human-readable title for the thread's chat session.
    pub session_title: String,
    /// The `YYYY-MM-DD` actually used in the thread id.
    pub date: String,
}

/// Resolve the per-meeting thread id + display title — the single owner of
/// meeting-thread naming for both rails. `MEET_BOT_THREAD` env still forces one
/// fixed thread (escape hatch / tests). `date` is used when it parses as
/// `YYYY-MM-DD`, else the local date. With neither a URL nor a title the
/// meeting keys to the dated ad-hoc thread (`meeting-ad-hoc-<date>`).
pub fn resolve_meeting_thread(
    meet_url: Option<&str>,
    title: Option<&str>,
    date: Option<&str>,
) -> ResolvedMeetingThread {
    let date = date
        .map(str::trim)
        .filter(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok())
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
    let url = meet_url.map(str::trim).filter(|u| !u.is_empty());
    let title = title.map(str::trim).filter(|t| !t.is_empty());
    let thread = std::env::var("MEET_BOT_THREAD")
        .ok()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| match (url, title) {
            (None, None) => format!("meeting-ad-hoc-{date}"),
            (u, t) => derive_meeting_thread_id(u.unwrap_or(""), t, &date),
        });
    let session_title = match (title, url) {
        (Some(t), _) => format!("{t} — {date}"),
        (None, Some(u)) => format!(
            "Meet {} — {date}",
            u.trim_start_matches("https://")
                .trim_start_matches("http://")
        ),
        (None, None) => format!("Ad-hoc meeting — {date}"),
    };
    ResolvedMeetingThread {
        thread,
        session_title,
        date,
    }
}

struct MeetingState {
    status: MeetingStatus,
    transcript: Vec<TranscriptTurn>,
    latest_summary: Option<String>,
    turns_since_summary: usize,
}

/// Owns one live meeting. One session per meeting (cf. `VoiceOrchestrator`).
///
/// Written against injectable seams (STT ears, capture/inject audio, responder);
/// all default to no-ops so the session is testable before the native bridge,
/// a real responder, and the agent-browser join land.
pub struct MeetingSession {
    config: MeetingConfig,
    summarizer: Arc<dyn Summarizer>,
    stt: Arc<dyn StreamingSttProvider>,
    source: Arc<dyn AudioSource>,
    sink: Arc<dyn AudioSink>,
    responder: Arc<dyn MeetingResponder>,
    /// TTS for short spoken cues (the "on it" ack + the "still busy" notice).
    /// `None` = cues are silent (e.g. tests). Distinct from the responder's voice.
    tts: Option<Arc<dyn TtsProvider>>,
    tts_voice: Option<String>,
    /// Set while a reply is in flight, so a new wake is acknowledged-but-ignored
    /// (we never queue at the agent or overlap audio).
    responding: AtomicBool,
    /// True ONLY while a *reply* is playing (NOT the cue). Gates barge-in: any
    /// STT activity heard while this is set is a human talking over the bot
    /// (under app-filtered capture, the downlink can't carry the bot's own
    /// voice; under display capture, half-duplex mutes the ears instead).
    speaking: AtomicBool,
    /// True while ANY sink playback is in flight (cue or reply). Under
    /// half-duplex it gates the capture pump so the bot never hears itself.
    sink_playing: AtomicBool,
    /// Set when the capture target is whole-display audio, which hears the
    /// bot's own injected voice: the capture pump drops chunks while
    /// `sink_playing` so the bot can't transcribe / wake / barge-in on itself.
    /// Trade-off: barge-in is unavailable in this mode (the ears are muted
    /// exactly while a reply plays).
    half_duplex: AtomicBool,
    /// Coalesces the off-loop rolling summaries (at most one in flight) — see
    /// [`Self::spawn_resummarize`].
    summarize_inflight: Arc<AtomicBool>,
    /// "Mute Magician": while set, the capture pump drops chunks before STT —
    /// the bot hears nothing (no transcript, no wake, no replies) until
    /// unmuted. Capture keeps running so unmute is instant.
    paused: Arc<AtomicBool>,
    /// The current reply's interrupt token. Cancelled by [`maybe_barge_in`] to
    /// drop the in-flight reply `play` future (which kills the inject helper).
    interrupt: Mutex<Option<CancellationToken>>,
    /// Serializes all sink playback so cues never overlap an answer.
    play_lock: Mutex<()>,
    /// Rate-limits the spoken "still busy" notice.
    last_busy_notice: Mutex<Option<Instant>>,
    /// Fired by [`stop()`](Self::stop) to halt capture + the listen loop. Phase 0
    /// is a hard stop (drop the capture future, break the loop, mark `Left`);
    /// Phase 1 will make teardown graceful (e.g. a spoken summary before leaving).
    cancel: CancellationToken,
    /// Persists the meeting's takeaways to memory on teardown (Phase 1b). No-op by
    /// default; the `meeting` tool injects a scoped writer when an agent scope is
    /// present (terminal runs / tests keep the no-op).
    memory_writer: Arc<dyn MeetingMemoryWriter>,
    /// Joins/leaves the meeting in a browser (Phase 2 auto-join). No-op by default
    /// (`NoopBrowserJoin`) → the bot rides an already-open browser (manual-join);
    /// the `meeting` tool injects the macOS `AgentBrowserMeetJoiner` so the bot
    /// launches + joins its own headed browser and capture targets that PID.
    browser: Arc<dyn BrowserJoin>,
    /// Streams every finalized turn into the bot's per-meeting chat thread as a
    /// display-only line (Phase 5 live transcript). No-op by default; the
    /// `meeting` tool injects a `ChatThreadTranscriptSink`. It posts to the
    /// non-dispatching transcript endpoint, so streaming can NEVER trigger a
    /// reply — only the wake-phrase gate does.
    transcript_sink: Arc<dyn TranscriptSink>,
    /// The JOIN-time thread resolution (set by `manager::join`). Teardown
    /// provenance must reuse this instead of re-resolving: with no explicit
    /// `meeting_date` the resolver substitutes TODAY, so a fresh teardown-time
    /// resolution of a meeting that crossed midnight points at tomorrow's
    /// thread — one the transcript never streamed into.
    resolved_thread: Option<ResolvedMeetingThread>,
    state: Arc<Mutex<MeetingState>>,
}

impl MeetingSession {
    pub fn new(config: MeetingConfig, summarizer: Arc<dyn Summarizer>) -> Self {
        Self {
            config,
            summarizer,
            stt: Arc::new(NoopStreamingSttProvider),
            source: Arc::new(NoopAudioSource),
            sink: Arc::new(NoopAudioSink),
            responder: Arc::new(NoopResponder),
            tts: None,
            tts_voice: None,
            responding: AtomicBool::new(false),
            speaking: AtomicBool::new(false),
            sink_playing: AtomicBool::new(false),
            half_duplex: AtomicBool::new(false),
            summarize_inflight: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            interrupt: Mutex::new(None),
            play_lock: Mutex::new(()),
            last_busy_notice: Mutex::new(None),
            cancel: CancellationToken::new(),
            memory_writer: Arc::new(NoopMeetingMemoryWriter),
            browser: Arc::new(NoopBrowserJoin),
            transcript_sink: Arc::new(NoopTranscriptSink),
            resolved_thread: None,
            state: Arc::new(Mutex::new(MeetingState {
                status: MeetingStatus::Idle,
                transcript: Vec::new(),
                latest_summary: None,
                turns_since_summary: 0,
            })),
        }
    }

    /// Inject the streaming "ears" (defaults to a no-op provider).
    pub fn with_stt(mut self, stt: Arc<dyn StreamingSttProvider>) -> Self {
        self.stt = stt;
        self
    }

    /// Inject the capture source + inject sink — the macOS native bridge.
    pub fn with_audio(mut self, source: Arc<dyn AudioSource>, sink: Arc<dyn AudioSink>) -> Self {
        self.source = source;
        self.sink = sink;
        self
    }

    /// Inject the responder (LLM+TTS today; the OpenAI-Realtime rail later).
    pub fn with_responder(mut self, responder: Arc<dyn MeetingResponder>) -> Self {
        self.responder = responder;
        self
    }

    /// Provide a TTS voice for short spoken cues (the ack + the busy notice).
    pub fn with_tts(mut self, tts: Arc<dyn TtsProvider>, voice: Option<String>) -> Self {
        self.tts = Some(tts);
        self.tts_voice = voice;
        self
    }

    /// Inject the memory writer that persists the meeting's takeaways on teardown
    /// (defaults to a no-op). The `meeting` tool supplies a scoped writer when an
    /// agent scope is available.
    pub fn with_memory_writer(mut self, writer: Arc<dyn MeetingMemoryWriter>) -> Self {
        self.memory_writer = writer;
        self
    }

    /// Inject the browser-join seam (Phase 2 auto-join). Defaults to
    /// [`NoopBrowserJoin`] (the bot rides an already-open browser); the `meeting`
    /// tool supplies the macOS `AgentBrowserMeetJoiner` so the bot launches +
    /// joins its own headed browser and capture is retargeted to that PID.
    pub fn with_browser_join(mut self, browser: Arc<dyn BrowserJoin>) -> Self {
        self.browser = browser;
        self
    }

    /// Inject the live-transcript sink (Phase 5). Defaults to
    /// [`NoopTranscriptSink`] (no streaming); the `meeting` tool supplies a
    /// `ChatThreadTranscriptSink` so every heard turn appears in the per-meeting
    /// chat thread without ever triggering a reply.
    pub fn with_transcript_sink(mut self, sink: Arc<dyn TranscriptSink>) -> Self {
        self.transcript_sink = sink;
        self
    }

    /// Pin the JOIN-time thread resolution so teardown provenance can't drift
    /// from the thread the transcript actually streamed into (see the field doc).
    pub fn with_resolved_thread(mut self, resolved: ResolvedMeetingThread) -> Self {
        self.resolved_thread = Some(resolved);
        self
    }

    pub async fn status(&self) -> MeetingStatus {
        self.state.lock().await.status
    }

    pub async fn set_status(&self, status: MeetingStatus) {
        self.state.lock().await.status = status;
    }

    /// "Mute Magician": gate the capture pump (see the `paused` field doc).
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Record a finalized transcript turn (called by the streaming-STT wiring).
    /// Triggers a rolling re-summary once enough new turns accumulate.
    pub async fn record_turn(&self, turn: TranscriptTurn) {
        // Stream the heard turn into the per-meeting chat thread (display-only —
        // the sink ENQUEUES without blocking, and the transcript endpoint never
        // dispatches the agent, so this can neither stall the listen loop nor
        // trigger a reply). No-op unless the `meeting` tool injected a chat sink.
        self.transcript_sink.post_turn(&turn).await;
        self.record_turn_local(turn).await;
    }

    /// Record the bot's OWN spoken reply: local transcript/summary context only,
    /// NOT streamed to the chat thread — the responder lane already persisted the
    /// reply as an assistant message there, so streaming it again would double-
    /// post every reply.
    async fn record_own_turn(&self, turn: TranscriptTurn) {
        self.record_turn_local(turn).await;
    }

    /// Append to the in-memory transcript + trigger the rolling re-summary.
    /// The summary runs OFF-loop (`spawn_resummarize`): a full-transcript local
    /// LLM call can take minutes, and awaiting it here would stall the very
    /// loop that gates wake responses and barge-in.
    async fn record_turn_local(&self, turn: TranscriptTurn) {
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

    /// Rolling re-summary on its own task, coalesced (at most one in flight).
    /// A summary completing after teardown started (cancel fired) is discarded
    /// so it can't clobber the awaited FINAL teardown summary with an older
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
                st.transcript
                    .iter()
                    .map(turn_line)
                    .collect::<Vec<_>>()
                    .join("\n")
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
                        tracing::warn!(target: "meet_bot", error = %err, "meeting summarization failed");
                    },
                }
            }
            inflight.store(false, Ordering::SeqCst);
        });
    }

    /// Re-run the summarizer over the full transcript so far and cache it.
    pub async fn resummarize(&self) {
        let transcript = self.transcript_text().await;
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
                tracing::warn!(target: "meet_bot", error = %err, "meeting summarization failed");
            },
        }
    }

    pub async fn latest_summary(&self) -> Option<String> {
        self.state.lock().await.latest_summary.clone()
    }

    /// Flattened transcript text (chronological).
    pub async fn transcript_text(&self) -> String {
        let st = self.state.lock().await;
        st.transcript
            .iter()
            .map(turn_line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Context handed to the responder when the bot is addressed: the latest
    /// summary (if any) + the recent transcript tail.
    pub async fn responder_context(&self) -> String {
        let st = self.state.lock().await;
        let mut out = String::new();
        if let Some(summary) = &st.latest_summary {
            out.push_str("Meeting summary so far:\n");
            out.push_str(summary);
            out.push_str("\n\n");
        }
        out.push_str("Recent transcript:\n");
        let start = st
            .transcript
            .len()
            .saturating_sub(self.config.responder_tail_turns);
        for turn in &st.transcript[start..] {
            out.push_str(&turn_line(turn));
            out.push('\n');
        }
        out
    }

    /// True if `utterance` contains one of the configured wake phrases. (The
    /// wake-word detector is the low-latency path; this is the transcript-level
    /// fallback / unit-testable gate.)
    pub fn is_addressed(&self, utterance: &str) -> bool {
        let lower = utterance.to_lowercase();
        self.config
            .wake_phrases
            .iter()
            .any(|phrase| lower.contains(&phrase.to_lowercase()))
    }

    /// Run the meeting loop: capture → STT → record turns (rolling summary on
    /// cadence) and respond when a wake phrase fires. Returns when the STT event
    /// stream closes (meeting over). Takes `Arc<Self>` so capture + the STT pump
    /// run as spawned tasks.
    ///
    /// Remaining wiring (separate steps): the native Core-Audio bridge behind
    /// `AudioSource`/`AudioSink`, a real `MeetingResponder` (LLM+TTS or the
    /// OpenAI-Realtime rail), a low-latency wake-word detector (today the gate
    /// is `is_addressed()` over finalized transcript), and the agent-browser
    /// join (the `meet` engine profile).
    pub async fn start(self: Arc<Self>) {
        self.set_status(MeetingStatus::Joining).await;
        tracing::info!(
            target: "meet_bot",
            url = %self.config.meet_url,
            "MeetingSession.start"
        );

        // Auto-join (Phase 2): drive the `BrowserJoin` seam before any capture.
        // `NoopBrowserJoin` returns immediately with no capture target, preserving
        // the manual-join path (capture keeps its bundle-id default). The real
        // `AgentBrowserMeetJoiner` launches its own browser and hands back the
        // launched Chromium's PID so capture targets exactly that instance.
        let joined = match self
            .browser
            .join(&self.config.meet_url, &self.config.display_name)
            .await
        {
            Ok(j) => j,
            Err(error) => {
                tracing::error!(target: "meet_bot", %error, "meeting join failed");
                self.set_status(MeetingStatus::Failed).await;
                return;
            },
        };
        if let Some(target) = joined.capture_target.clone() {
            // Retarget capture to the just-launched browser (no-op join leaves
            // this unset → capture keeps its bundle-id default).
            if matches!(target, CaptureTarget::DisplayAudio) {
                // Whole-display capture hears the bot's OWN injected voice — run
                // half-duplex (the pump drops chunks while the sink plays) so it
                // can't transcribe / wake / barge-in on itself. Barge-in is
                // unavailable in this mode by construction.
                self.half_duplex.store(true, Ordering::SeqCst);
                tracing::info!(
                    target: "meet_bot",
                    "display-audio capture → half-duplex (ears muted while the bot speaks; barge-in unavailable)"
                );
            }
            self.source.set_capture_target(target).await;
        }

        // Removed-detection poll (Phase 2): if the browser is no longer in the
        // meeting (host ended it / removed the bot), fire the unified cancel so
        // teardown runs. Exits on cancel too, so it never leaks past teardown.
        // For `NoopBrowserJoin`, `is_in_meeting()` is always true → this idles
        // until cancel, then exits (manual-join behavior preserved).
        {
            let me = self.clone();
            let cancel = self.cancel.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {
                            if !me.browser.is_in_meeting().await {
                                tracing::info!(
                                    target: "meet_bot",
                                    "browser no longer in meeting; tearing down"
                                );
                                cancel.cancel();
                                break;
                            }
                        }
                    }
                }
            });
        }

        // Open the streaming-STT "ears".
        let (evt_tx, mut evt_rx) = mpsc::channel::<StreamingSttEvent>(64);
        let stt_session = match self
            .stt
            .open_session(StreamAudioFormat::default(), evt_tx)
            .await
        {
            Ok(session) => session,
            Err(err) => {
                tracing::error!(target: "meet_bot", error = %err, "failed to open streaming STT session");
                // The browser already joined — leave (clicks hang-up, closes the
                // browser, restores the host's mic) instead of stranding the bot
                // in the call with the default input still flipped. stop() can't
                // recover this case: it only cancels a listen loop that never ran.
                self.browser.leave().await;
                self.set_status(MeetingStatus::Failed).await;
                return;
            },
        };

        // Rolling buffer of recent capture PCM so a realtime responder can be
        // handed the just-spoken question audio (the wake gate only fires after
        // the utterance has finished). Pruned to AUDIO_RING_RETAIN.
        let audio_ring: Arc<Mutex<VecDeque<(Instant, Bytes)>>> =
            Arc::new(Mutex::new(VecDeque::new()));

        // Capture: AudioSource → chunk channel. Cancellable: on stop(), drop the
        // capture future so `chunk_tx` drops → the pump ends → the STT finishes.
        let (chunk_tx, mut chunk_rx) = mpsc::channel::<AudioChunk>(64);
        {
            let source = self.source.clone();
            let cancel = self.cancel.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    result = source.run(chunk_tx) => {
                        if let Err(err) = result {
                            tracing::warn!(target: "meet_bot", error = %err, "audio source ended with error");
                        }
                    }
                }
            });
        }

        // Pump: chunk channel → ring buffer (tee) + STT session; flush when
        // capture ends. The session is MOVED here (not kept by `start`), so its
        // STT event sender drops when capture ends, which closes `evt_rx` and
        // ends the loop.
        {
            let audio_ring = audio_ring.clone();
            let me = self.clone();
            tokio::spawn(async move {
                while let Some(chunk) = chunk_rx.recv().await {
                    // "Mute Magician": drop chunks before STT — the bot hears
                    // nothing until unmuted.
                    if me.paused.load(Ordering::SeqCst) {
                        continue;
                    }
                    // Half-duplex (display-audio capture): the captured mix
                    // includes the bot's own injected voice — drop chunks while
                    // the sink plays so the bot never hears itself.
                    if me.half_duplex.load(Ordering::SeqCst)
                        && me.sink_playing.load(Ordering::SeqCst)
                    {
                        continue;
                    }
                    {
                        let mut ring = audio_ring.lock().await;
                        ring.push_back((Instant::now(), chunk.pcm.clone()));
                        while let Some((ts, _)) = ring.front() {
                            if ts.elapsed() > AUDIO_RING_RETAIN {
                                ring.pop_front();
                            } else {
                                break;
                            }
                        }
                    }
                    if let Err(err) = stt_session.push_audio(chunk).await {
                        tracing::warn!(target: "meet_bot", error = %err, "stt push_audio failed");
                        break;
                    }
                }
                let _ = stt_session.finish().await;
            });
        }

        self.set_status(MeetingStatus::Listening).await;

        // Consume transcript events. Respond on an addressed FINAL, or — for
        // engines that stream partials but never emit a final on continuous
        // audio (on-device macOS Speech) — on an addressed PARTIAL that has
        // SETTLED (no new event for a short debounce). Ends when the stream closes.
        const SETTLE: Duration = Duration::from_millis(1500);
        // The recognizer keeps re-emitting an un-finalized utterance; ignore a
        // re-detected identical utterance within this window so the same query
        // doesn't trigger the agent twice.
        const RESPOND_COOLDOWN: Duration = Duration::from_secs(8);
        let mut pending: Option<String> = None;
        let mut last_responded: Option<(String, Instant)> = None;
        // Approx start of the current utterance (first partial after silence), so
        // we can pull just the question audio from the ring buffer on a wake.
        let mut utterance_started_at: Option<Instant> = None;
        loop {
            let recv = tokio::select! {
                _ = self.cancel.cancelled() => break,
                recv = tokio::time::timeout(SETTLE, evt_rx.recv()) => recv,
            };
            match recv {
                Ok(Some(StreamingSttEvent::Final {
                    text,
                    speaker,
                    start_ms,
                    ..
                })) => {
                    let text = text.trim().to_string();
                    if text.is_empty() {
                        continue;
                    }
                    // A human spoke (final) while the bot is mid-reply → stop the
                    // reply (barge-in). No-op unless a reply is currently playing.
                    self.maybe_barge_in().await;
                    pending = None;
                    let turn = TranscriptTurn {
                        at_ms: start_ms.unwrap_or(0),
                        speaker,
                        text: text.clone(),
                    };
                    tracing::info!(target: "meet_bot", "heard: {text}");
                    let mut responding = false;
                    if self.is_addressed(&text)
                        && !recently_said(&last_responded, &text, RESPOND_COOLDOWN)
                    {
                        let audio =
                            extract_utterance_audio(&audio_ring, utterance_started_at).await;
                        if self.try_spawn_response(text.clone(), audio) {
                            last_responded = Some((text, Instant::now()));
                            responding = true;
                        } else {
                            self.notify_busy();
                        }
                    }
                    // Chat-persisting responders surface the addressed exchange
                    // themselves — streaming it again would show every wake
                    // question twice in the thread. Everything else (unaddressed
                    // turns; all turns under local-only responders) goes through
                    // the transcript sink.
                    if responding && self.responder.persists_to_chat() {
                        self.record_own_turn(turn).await;
                    } else {
                        self.record_turn(turn).await;
                    }
                    utterance_started_at = None;
                },
                Ok(Some(StreamingSttEvent::Partial { text, .. })) => {
                    let text = text.trim().to_string();
                    if !text.is_empty() {
                        // Earliest signal a human is speaking: if it lands while
                        // the bot is mid-reply, stop the reply (barge-in).
                        self.maybe_barge_in().await;
                        if utterance_started_at.is_none() {
                            utterance_started_at = Some(Instant::now());
                        }
                    }
                    tracing::debug!(target: "meet_bot", "partial: {text}");
                    if !text.is_empty()
                        && self.is_addressed(&text)
                        && !recently_said(&last_responded, &text, RESPOND_COOLDOWN)
                    {
                        pending = Some(text);
                    }
                },
                Ok(Some(StreamingSttEvent::Error { reason })) => {
                    tracing::warn!(target: "meet_bot", reason = %reason, "streaming STT error event");
                },
                Ok(None) => break, // stream closed
                Err(_) => {
                    // Debounce elapsed: an addressed partial settled into a
                    // complete utterance → record + respond (unless it's a repeat
                    // of the utterance we just answered).
                    if let Some(text) = pending.take() {
                        if !recently_said(&last_responded, &text, RESPOND_COOLDOWN) {
                            let turn = TranscriptTurn {
                                at_ms: 0,
                                speaker: None,
                                text: text.clone(),
                            };
                            tracing::info!(target: "meet_bot", "heard (settled): {text}");
                            let audio =
                                extract_utterance_audio(&audio_ring, utterance_started_at).await;
                            let mut responding = false;
                            if self.try_spawn_response(text.clone(), audio) {
                                last_responded = Some((text, Instant::now()));
                                responding = true;
                            } else {
                                self.notify_busy();
                            }
                            // Same lane routing as the Final branch: a settled
                            // utterance is always addressed, so skip the sink
                            // when the responder will surface it itself.
                            if responding && self.responder.persists_to_chat() {
                                self.record_own_turn(turn).await;
                            } else {
                                self.record_turn(turn).await;
                            }
                        }
                    }
                    utterance_started_at = None;
                },
            }
        }

        // Leave the call FIRST — hang up, close the browser, restore the host's
        // audio. The user-visible effect of "leave the meeting" must not wait on
        // summarization: the final resummarize is a local-LLM call over the whole
        // transcript and can take MINUTES, during which the bot used to sit in
        // the call as a zombie participant. Nothing below reads the page — the
        // summary works off the in-memory transcript. No-op for `NoopBrowserJoin`.
        self.browser.leave().await;

        // Fence stragglers (idempotent): in-flight rolling summaries must not
        // clobber the FINAL summary computed below.
        self.cancel.cancel();

        // Drain the flushed tail: stopping capture triggers an STT flush whose
        // finals land AFTER the listen loop exits (the flush is itself a
        // network call), so without this bounded drain the meeting's closing
        // utterances would be missing from the transcript, summary, and
        // memory write.
        let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        loop {
            match tokio::time::timeout_at(drain_deadline, evt_rx.recv()).await {
                Ok(Some(StreamingSttEvent::Final {
                    text,
                    speaker,
                    start_ms,
                    ..
                })) => {
                    let text = text.trim().to_string();
                    if !text.is_empty() {
                        tracing::info!(target: "meet_bot", "heard (tail): {text}");
                        self.record_turn(TranscriptTurn {
                            at_ms: start_ms.unwrap_or(0),
                            speaker,
                            text,
                        })
                        .await;
                    }
                },
                Ok(Some(_)) => {},
                Ok(None) | Err(_) => break,
            }
        }

        // Final teardown summary (Phase 1a): capture the freshest summary of the
        // whole meeting. resummarize() is a no-op on an empty transcript and
        // logs+swallows summarizer errors, so this is safe.
        self.resummarize().await;

        // Persist the meeting's takeaways to memory (Phase 1b): parse the final
        // prose summary into `fields` and hand them to the injected writer (no-op
        // unless the `meeting` tool supplied a scoped writer).
        if let Some(summary) = self.latest_summary().await {
            // The thread is the durable record — post the final summary INTO
            // it. `latest_summary` otherwise lives only in this process's
            // session registry and disappears once the ended session is
            // reaped (same "summary visible, then gone" bug as the passive
            // rail).
            self.transcript_sink
                .post_turn(&TranscriptTurn {
                    at_ms: 0,
                    speaker: Some("Meeting summary".to_string()),
                    text: summary.clone(),
                })
                .await;
            let fields = super::meeting_memory::meeting_summary_to_fields(&summary);
            // Prefer the pinned join-time resolution; re-resolve only on
            // seam-level runs that never set it (the fresh resolution can
            // point at the wrong date after a midnight crossing).
            let resolved = self.resolved_thread.clone().unwrap_or_else(|| {
                resolve_meeting_thread(
                    Some(&self.config.meet_url),
                    self.config.title.as_deref(),
                    self.config.meeting_date.as_deref(),
                )
            });
            let meta = super::meeting_memory::MeetingMemoryMeta {
                thread_id: Some(resolved.thread),
                mode: "attendee",
                title: self.config.title.clone(),
                url: Some(self.config.meet_url.clone()),
                date: Some(resolved.date),
            };
            self.memory_writer.write_summary(fields, &meta).await;
        }

        self.set_status(MeetingStatus::Left).await;
    }

    /// Try to start a reply for `utterance`. Returns false if a reply is already
    /// in flight — the caller acknowledges-but-skips (never queues at the agent).
    fn try_spawn_response(
        self: &Arc<Self>,
        utterance: String,
        audio: Option<UtteranceAudio>,
    ) -> bool {
        if self
            .responding
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        let me = self.clone();
        tokio::spawn(async move {
            me.clone().respond_to(utterance, audio).await;
            me.responding.store(false, Ordering::SeqCst);
        });
        true
    }

    /// Speak a short "still busy" notice (rate-limited), then ignore the ask.
    fn notify_busy(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            {
                let mut last = me.last_busy_notice.lock().await;
                if matches!(*last, Some(when) if when.elapsed() < Duration::from_secs(6)) {
                    return;
                }
                *last = Some(Instant::now());
            }
            me.cue("Still on the last request — one moment.").await;
        });
    }

    /// Draft + speak a reply to a wake-phrase utterance, then inject it. Runs on a
    /// spawned task (the listen loop keeps running); `responding` gates re-entry.
    async fn respond_to(self: Arc<Self>, utterance: String, audio: Option<UtteranceAudio>) {
        self.set_status(MeetingStatus::Responding).await;
        self.cue("On it.").await; // fast ack so there's no dead air while it works
        let context = self.responder_context().await;
        match self
            .responder
            .respond_with_audio(&utterance, audio.as_ref(), &context)
            .await
        {
            Ok(reply) if !reply.is_empty() => {
                if !reply.text.trim().is_empty() {
                    // Record the bot's own turn so later context stays coherent.
                    // Chat-persisting responders already surfaced the reply in the
                    // thread (record locally only); local-only responders
                    // (llm-tts / realtime-direct) have no chat lane, so the
                    // transcript sink is the reply's ONLY route into the thread —
                    // skipping it would make the transcript read one-sided.
                    let turn = TranscriptTurn {
                        at_ms: 0,
                        speaker: Some(self.config.display_name.clone()),
                        text: reply.text.clone(),
                    };
                    if self.responder.persists_to_chat() {
                        self.record_own_turn(turn).await;
                    } else {
                        self.record_turn(turn).await;
                    }
                    tracing::info!(target: "meet_bot", "{}: {}", self.config.display_name, reply.text);
                }
                // Make the reply play cancellable: a human talking over it fires
                // the token (see `maybe_barge_in`), the `play` future is dropped,
                // and the inject helper is killed (`kill_on_drop`) so audio stops.
                // `speaking` is gated to THIS play (not the cue) so the wake
                // utterance's tail doesn't self-interrupt.
                let token = CancellationToken::new();
                *self.interrupt.lock().await = Some(token.clone());
                self.speaking.store(true, Ordering::SeqCst);
                tokio::select! {
                    _ = self.play(reply.pcm, reply.format) => {}
                    _ = token.cancelled() => {
                        tracing::info!(target: "meet_bot", "barge-in: human spoke over the reply; stopping playback");
                    }
                }
                self.speaking.store(false, Ordering::SeqCst);
                *self.interrupt.lock().await = None;
            },
            Ok(_) => {
                tracing::debug!(target: "meet_bot", "responder declined to speak");
            },
            Err(err) => {
                tracing::warn!(target: "meet_bot", error = %err, "responder failed");
            },
        }
        self.set_status(MeetingStatus::Listening).await;
    }

    /// Barge-in: if a reply is currently playing and a human is heard talking
    /// over it, cancel the reply's interrupt token. The cancelled token drops the
    /// in-flight `play` future, which kills the inject helper so the bot goes
    /// silent. No-op when the bot isn't speaking. The trigger is correct under
    /// app-filtered capture by topology (the captured downlink can't carry the
    /// bot's own injected voice); under display-audio capture the half-duplex
    /// pump gate means no STT events arrive while a reply plays, so this simply
    /// never fires there (barge-in unavailable, never self-triggered).
    async fn maybe_barge_in(&self) {
        if self.speaking.load(Ordering::SeqCst) {
            if let Some(tok) = self.interrupt.lock().await.as_ref() {
                tok.cancel();
            }
        }
    }

    /// Test-only: whether a reply is currently playing (barge-in window).
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn is_speaking(&self) -> bool {
        self.speaking.load(Ordering::SeqCst)
    }

    /// Synthesize a short cue phrase and play it (no-op if no cue TTS is set).
    async fn cue(&self, phrase: &str) {
        let Some(tts) = self.tts.as_ref() else {
            return;
        };
        let request = TtsRequest {
            text: phrase.to_string(),
            voice: self.tts_voice.clone(),
            rate: None,
            model: None,
            format: Some("pcm".to_string()),
            message_id: None,
            emotion: None,
            style: None,
            pace: None,
            voice_mode: None,
            emphasis: None,
        };
        match tts.synthesize(request).await {
            Ok(resp) => {
                self.play(
                    resp.audio,
                    StreamAudioFormat {
                        sample_rate_hz: 24_000,
                        channels: 1,
                        sample_format: Default::default(),
                    },
                )
                .await
            },
            Err(err) => tracing::warn!(target: "meet_bot", error = %err, "cue tts failed"),
        }
    }

    /// Play PCM to the sink, serialized so cues never overlap an answer.
    /// Marks `sink_playing` for the duration (RAII, so a barge-in-cancelled —
    /// dropped — play still clears it): under half-duplex the capture pump
    /// reads this to mute the ears while the bot's own audio is in flight.
    async fn play(&self, pcm: Bytes, format: StreamAudioFormat) {
        struct PlayingGuard<'a>(&'a AtomicBool);
        impl Drop for PlayingGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _guard = self.play_lock.lock().await;
        self.sink_playing.store(true, Ordering::SeqCst);
        let _playing = PlayingGuard(&self.sink_playing);
        if let Err(err) = self.sink.play_pcm(pcm, format).await {
            tracing::warn!(target: "meet_bot", error = %err, "audio sink play_pcm failed");
        }
    }

    /// Halt a running session. Fires the cancel signal so capture is dropped and
    /// the listen loop breaks; the loop then marks the session `Left` on exit.
    pub async fn stop(&self) {
        self.cancel.cancel();
    }
}

/// Pull the just-spoken utterance's PCM out of the rolling capture buffer, from
/// roughly the utterance start (minus a small lead) to now. Returns `None` when
/// nothing recent is buffered. Tagged with the capture format (the bridge emits
/// 16 kHz mono PCM16) so a realtime responder can resample as needed.
async fn extract_utterance_audio(
    ring: &Mutex<VecDeque<(Instant, Bytes)>>,
    started_at: Option<Instant>,
) -> Option<UtteranceAudio> {
    let since = match started_at {
        Some(t) => t.checked_sub(UTTERANCE_LEAD).unwrap_or(t),
        None => Instant::now().checked_sub(UTTERANCE_FALLBACK_WINDOW)?,
    };
    let ring = ring.lock().await;
    let mut pcm = Vec::new();
    for (ts, chunk) in ring.iter() {
        if *ts >= since {
            pcm.extend_from_slice(chunk);
        }
    }
    if pcm.len() < 2 {
        return None;
    }
    Some(UtteranceAudio {
        pcm: Bytes::from(pcm),
        format: StreamAudioFormat::default(),
    })
}

/// True if `text` matches the last-responded utterance within `cooldown` —
/// used to ignore the recognizer re-emitting an un-finalized utterance.
fn recently_said(last: &Option<(String, Instant)>, text: &str, cooldown: Duration) -> bool {
    match last {
        Some((prev, when)) => prev.eq_ignore_ascii_case(text) && when.elapsed() < cooldown,
        None => false,
    }
}

fn turn_line(turn: &TranscriptTurn) -> String {
    match &turn.speaker {
        Some(speaker) => format!("{}: {}", speaker, turn.text),
        None => turn.text.clone(),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::meeting_memory::MeetingMemoryWriter;
    use crate::magician_v2::media_seam::responder::{ResponderError, SpokenReply};
    use crate::magician_v2::media_seam::*;
    use crate::magician_v2::media_seam::{StreamingSttSession, SttError};
    use async_trait::async_trait;
    use bytes::Bytes;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{mpsc, Mutex};

    #[test]
    fn thread_id_prefers_slugified_title() {
        assert_eq!(
            derive_meeting_thread_id(
                "https://meet.google.com/abc-defg-hij",
                Some("Eng Standup / Daily"),
                "2026-06-10",
            ),
            "meeting-eng-standup-daily-2026-06-10",
        );
    }

    #[test]
    fn thread_id_falls_back_to_meet_code() {
        // No title, query string present → first path segment, stripped of query.
        assert_eq!(
            derive_meeting_thread_id(
                "https://meet.google.com/abc-defg-hij?authuser=0",
                None,
                "2026-06-10",
            ),
            "meeting-abc-defg-hij-2026-06-10",
        );
        // Blank title is treated as absent.
        assert_eq!(
            derive_meeting_thread_id("meet.google.com/abc-defg-hij", Some("   "), "2026-06-10"),
            "meeting-abc-defg-hij-2026-06-10",
        );
        // lookup-style links carry the meeting id in the SECOND segment.
        assert_eq!(
            derive_meeting_thread_id(
                "https://meet.google.com/lookup/dev-sync-alias",
                None,
                "2026-06-10",
            ),
            "meeting-dev-sync-alias-2026-06-10",
        );
    }

    #[test]
    fn thread_id_hashes_when_no_code() {
        // No parseable path segment → stable hash label, never empty.
        let id = derive_meeting_thread_id("https://meet.google.com", None, "2026-06-10");
        assert!(id.starts_with("meeting-"));
        assert!(id.ends_with("-2026-06-10"));
        assert_ne!(id, "meeting--2026-06-10");
    }

    #[test]
    fn thread_id_distinguishes_non_ascii_titles() {
        // Non-ASCII-only titles slugify to empty; the fallback must hash the
        // TITLE so two different such meetings get two different threads
        // (and don't collapse into hash-of-empty-url).
        let a = derive_meeting_thread_id("", Some("全体会議"), "2026-06-11");
        let b = derive_meeting_thread_id("", Some("साप्ताहिक बैठक"), "2026-06-11");
        assert_ne!(a, b);
        // Stable per title.
        assert_eq!(
            a,
            derive_meeting_thread_id("", Some("全体会議"), "2026-06-11")
        );
    }

    fn session() -> MeetingSession {
        // Offline stub (no Ollama/HTTP); production uses RouterSummarizer.
        MeetingSession::new(MeetingConfig::default(), Arc::new(StubSummarizer))
    }

    /// `AudioSource` whose `run()` blocks "forever": it sleeps ~1h so the session
    /// loop only ends if the capture future is dropped (i.e. on cancel).
    struct BlockingSource;

    #[async_trait]
    impl AudioSource for BlockingSource {
        async fn run(
            &self,
            _out: mpsc::Sender<AudioChunk>,
        ) -> Result<(), crate::magician_v2::media_seam::meeting_audio::AudioError> {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(())
        }
    }

    /// `StreamingSttProvider` whose opened session HOLDS the event sender so the
    /// listen loop never sees a closed stream (unlike the no-op STT, which closes
    /// immediately). The session therefore only ends on cancel.
    struct HoldingStt;

    #[async_trait]
    impl StreamingSttProvider for HoldingStt {
        fn id(&self) -> &str {
            "holding"
        }
        async fn open_session(
            &self,
            _format: StreamAudioFormat,
            events: mpsc::Sender<StreamingSttEvent>,
        ) -> Result<Box<dyn StreamingSttSession>, SttError> {
            Ok(Box::new(HoldingSttSession { _events: events }))
        }
    }

    struct HoldingSttSession {
        /// Held so `evt_rx` in `start()` stays open for the session's lifetime.
        _events: mpsc::Sender<StreamingSttEvent>,
    }

    #[async_trait]
    impl StreamingSttSession for HoldingSttSession {
        async fn push_audio(&self, _chunk: AudioChunk) -> Result<(), SttError> {
            Ok(())
        }
        async fn finish(&self) -> Result<(), SttError> {
            Ok(())
        }
    }

    /// Offline summarizer: returns a fixed sectioned summary so the teardown
    /// chain (summarize → parse → memory-write) runs with no Ollama/HTTP.
    struct StubSummarizer;

    #[async_trait]
    impl Summarizer for StubSummarizer {
        async fn summarize(
            &self,
            _transcript: &str,
        ) -> Result<String, crate::magician_v2::media_seam::meeting_summarizer::SummarizerError>
        {
            Ok("## Summary\nScoped the launch.\n\n## Decisions\n- Ship Friday\n\n## Action items\n- Aman: payments by Wed\n".to_string())
        }
    }

    /// Captures the parsed `fields` handed to the memory writer on teardown, so a
    /// test can assert the full summarize → parse → write chain ran offline.
    struct RecordingMemoryWriter {
        last: Arc<Mutex<Option<serde_json::Map<String, serde_json::Value>>>>,
    }

    #[async_trait]
    impl MeetingMemoryWriter for RecordingMemoryWriter {
        async fn write_summary(
            &self,
            fields: serde_json::Map<String, serde_json::Value>,
            _meta: &crate::magician_v2::media_seam::meeting_memory::MeetingMemoryMeta,
        ) {
            *self.last.lock().await = Some(fields);
        }
    }

    /// `AudioSink` whose `play_pcm` never returns (sleeps ~1h) — simulates a long
    /// reply still playing, so the session stays in the barge-in window until the
    /// `play` future is dropped by the interrupt branch of the `respond_to`
    /// `select!`.
    struct BlockingSink;

    #[async_trait]
    impl AudioSink for BlockingSink {
        async fn play_pcm(
            &self,
            _pcm: Bytes,
            _format: StreamAudioFormat,
        ) -> Result<(), crate::magician_v2::media_seam::meeting_audio::AudioError> {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(())
        }
    }

    /// Responder that always returns a non-empty reply, so `respond_to` reaches
    /// the (blocking) reply `play` and the session enters the barge-in window.
    struct StubResponder;

    #[async_trait]
    impl MeetingResponder for StubResponder {
        async fn respond(
            &self,
            _utterance: &str,
            _context: &str,
        ) -> Result<SpokenReply, ResponderError> {
            Ok(SpokenReply {
                text: "ok".into(),
                pcm: Bytes::from_static(b"\x00\x01\x00\x01"),
                format: StreamAudioFormat::default(),
            })
        }
    }

    /// STT double that scripts a barge-in: on open it emits an addressed `Final`
    /// (triggers a reply), then ~250ms later a `Partial` (a human talking over the
    /// reply → barge-in), and HOLDS the sender so the stream stays open (like
    /// `HoldingStt`). The session therefore only ends on cancel.
    struct ScriptedStt;

    #[async_trait]
    impl StreamingSttProvider for ScriptedStt {
        fn id(&self) -> &str {
            "scripted"
        }
        async fn open_session(
            &self,
            _format: StreamAudioFormat,
            events: mpsc::Sender<StreamingSttEvent>,
        ) -> Result<Box<dyn StreamingSttSession>, SttError> {
            let scripted = events.clone();
            tokio::spawn(async move {
                // Addressed wake utterance → triggers a reply (which then plays on
                // the BlockingSink, keeping `speaking` true).
                let _ = scripted
                    .send(StreamingSttEvent::Final {
                        text: "hey magical status".into(),
                        speaker: None,
                        language: None,
                        start_ms: None,
                    })
                    .await;
                // Let the reply spawn + reach the (blocking) play.
                tokio::time::sleep(Duration::from_millis(250)).await;
                // A human talks over the reply → barge-in fires on this partial.
                let _ = scripted
                    .send(StreamingSttEvent::Partial {
                        text: "someone talking over".into(),
                        speaker: None,
                    })
                    .await;
                // Hold the scripted sender so the spawned task (and thus this
                // clone) lives for the test; the session-held `events` keeps the
                // stream open regardless.
                std::future::pending::<()>().await;
            });
            Ok(Box::new(HoldingSttSession { _events: events }))
        }
    }

    #[test]
    fn wake_phrase_match_is_case_insensitive() {
        let s = session();
        assert!(s.is_addressed("ok everyone, Hey magical can you summarize?"));
        assert!(s.is_addressed("HEY MAGICAN what did we decide"));
        assert!(!s.is_addressed("let's discuss the pricing page"));
    }

    #[tokio::test]
    async fn transcript_and_context_build() {
        let s = session();
        s.record_turn(TranscriptTurn {
            at_ms: 0,
            speaker: Some("Aman".to_string()),
            text: "payment gateway is done".to_string(),
        })
        .await;
        s.record_turn(TranscriptTurn {
            at_ms: 1000,
            speaker: None,
            text: "but there is a refund bug".to_string(),
        })
        .await;
        let text = s.transcript_text().await;
        assert!(text.contains("Aman: payment gateway is done"));
        assert!(text.contains("but there is a refund bug"));
        assert!(s.responder_context().await.contains("Recent transcript:"));
    }

    #[tokio::test]
    async fn start_with_noop_deps_reaches_left() {
        // The no-op STT provider drops its event sender on open, so the loop
        // sees an immediately-closed stream and exits cleanly (no hang).
        let s = Arc::new(session());
        s.clone().start().await;
        assert_eq!(s.status().await, MeetingStatus::Left);
    }

    #[tokio::test]
    async fn stop_cancels_a_running_session() {
        // A genuinely-running session: capture blocks forever and the STT holds
        // the event stream open, so `start()` would never return on its own.
        // `stop()` must fire the cancel signal and actually halt it.
        let s = Arc::new(
            session()
                .with_stt(Arc::new(HoldingStt))
                .with_audio(Arc::new(BlockingSource), Arc::new(NoopAudioSink)),
        );
        let handle = tokio::spawn(s.clone().start());

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(s.status().await, MeetingStatus::Listening);

        s.stop().await;

        // The start task must return promptly once cancelled.
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("start() did not return within 2s after stop()")
            .expect("start task panicked");

        assert_eq!(s.status().await, MeetingStatus::Left);
    }

    #[tokio::test]
    async fn teardown_writes_parsed_summary_to_memory() {
        // Exercises the FULL teardown chain offline: on stop, the listen loop
        // exits → resummarize() (StubSummarizer) → parse → memory writer →
        // Left. start() returns only AFTER that chain completes.
        let recorded = Arc::new(Mutex::new(None));
        let writer = Arc::new(RecordingMemoryWriter {
            last: recorded.clone(),
        });
        let s = Arc::new(
            MeetingSession::new(MeetingConfig::default(), Arc::new(StubSummarizer))
                .with_stt(Arc::new(HoldingStt))
                .with_audio(Arc::new(BlockingSource), Arc::new(NoopAudioSink))
                .with_memory_writer(writer),
        );
        // Give resummarize() non-empty content (it's a no-op on an empty transcript).
        s.record_turn(TranscriptTurn {
            at_ms: 0,
            speaker: None,
            text: "we talked".into(),
        })
        .await;

        let h = tokio::spawn(s.clone().start());
        tokio::time::sleep(Duration::from_millis(100)).await;
        s.stop().await;
        h.await.unwrap();

        let fields = recorded
            .lock()
            .await
            .clone()
            .expect("memory writer was called on teardown");
        let decisions = fields
            .get("decisions")
            .and_then(|v| v.as_array())
            .expect("decisions");
        assert_eq!(decisions[0].as_str(), Some("Ship Friday"));
        let actions = fields
            .get("action_items")
            .and_then(|v| v.as_array())
            .expect("action_items");
        assert_eq!(actions[0].as_str(), Some("Aman: payments by Wed"));
        assert_eq!(s.status().await, MeetingStatus::Left);
    }

    #[tokio::test]
    async fn barge_in_stops_the_reply() {
        // Full-loop barge-in: the scripted STT fires an addressed `Final` → a
        // reply spawns and starts playing on the BlockingSink (which never
        // returns), so `speaking` becomes true. ~250ms later the scripted
        // `Partial` lands → `maybe_barge_in` cancels the interrupt token → the
        // reply `play` future is dropped (the `select!` interrupt branch wins) →
        // `speaking` returns to false. We assert both transitions.
        //
        // No cue TTS is wired, so `cue("On it")` is a no-op and `speaking` is
        // never set for the cue — only the reply play flips it.
        let s = Arc::new(
            MeetingSession::new(MeetingConfig::default(), Arc::new(StubSummarizer))
                .with_stt(Arc::new(ScriptedStt))
                .with_audio(Arc::new(BlockingSource), Arc::new(BlockingSink))
                .with_responder(Arc::new(StubResponder)),
        );
        let h = tokio::spawn(s.clone().start());

        // Wait until the reply is playing (barge-in window open).
        let mut saw_speaking = false;
        for _ in 0..200 {
            if s.is_speaking() {
                saw_speaking = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            saw_speaking,
            "the reply never started playing (speaking never went true)"
        );

        // The scripted partial must fire barge-in → reply play dropped → not speaking.
        let mut stopped = false;
        for _ in 0..200 {
            if !s.is_speaking() {
                stopped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            stopped,
            "barge-in did not stop the reply (speaking stayed true)"
        );

        s.stop().await;
        let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
    }
}
