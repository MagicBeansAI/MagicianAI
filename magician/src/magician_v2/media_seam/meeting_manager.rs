//! Process-global registry that owns live `MeetingSession`s so a fire-and-forget
//! tool call (the `meeting` capability) can start an hour-long meeting and later
//! check / stop it by `session_id`. Phase 0: in-process in the magician server
//! (Approach A); the same interface is what Approach B moves behind the desktop
//! tray host-gateway later. See
//! `docs/plans/2026-06-09-meet-bot-capability-and-autojoin-design.md`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use once_cell::sync::Lazy;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::magician_v2::media_seam::{MeetingSession, MeetingStatus};

/// How long an ended session stays queryable before it's reaped. Public so a
/// surface that renders retained sessions can state the window instead of
/// guessing why a row vanished. Distinct from the passive rail's own constant
/// so neither glob re-export shadows the other.
pub const ATTENDEE_ENDED_RETAIN_SECS: u64 = 300;

struct Managed {
    session: Arc<MeetingSession>,
    url: String,
    /// The per-meeting chat thread + display title (None for seam-level spawns
    /// that bypass `join`, e.g. tests).
    thread: Option<String>,
    title: Option<String>,
    /// When the session was registered. A monotonic `Instant`, not a wall clock:
    /// capture duration must not jump when the host clock is adjusted, and the
    /// registries are process-local so an `Instant` never has to outlive them.
    started_at: Instant,
    /// The scope that started this capture, when one was supplied. The
    /// registry is keyed by session id alone, so without this a caller holding
    /// any session id could act on any scope's capture. `None` for seam-level
    /// spawns with no scope (tests); a scope-checked caller must refuse those
    /// rather than treat "unknown" as "mine".
    scope: Option<(String, String)>,
    ended_at: Option<Instant>,
    /// Notified once the spawned `start()` task returns — i.e. after the final
    /// teardown `resummarize()` + memory write have run. `leave` awaits this so it
    /// returns the POST-teardown final summary, not the rolling one.
    done: Arc<tokio::sync::Notify>,
}

#[derive(Debug, Clone)]
pub struct MeetingSummaryRow {
    pub session_id: String,
    pub status: MeetingStatus,
    pub url: String,
    /// Per-meeting chat thread id (where the transcript + replies land).
    pub thread: Option<String>,
    /// Human-readable meeting title.
    pub title: Option<String>,
    /// "Mute Magician" state (capture gated; the bot hears nothing).
    pub paused: bool,
    /// Freshest cached summary (same field the status view carries, so list
    /// and status consumers see one shape).
    pub latest_summary: Option<String>,
    /// Whole seconds since the session was registered — the capture duration a
    /// surface shows. Derived from a monotonic clock at read time.
    pub started_seconds_ago: u64,
    /// Whole seconds since teardown returned, or `None` while the session is
    /// still running. An ended session stays queryable for
    /// [`ATTENDEE_ENDED_RETAIN_SECS`] and then disappears, so a surface that renders
    /// session state owes the operator this age rather than a bare status.
    pub ended_seconds_ago: Option<u64>,
    /// `(principal, workspace)` the capture was started under, when known.
    pub scope: Option<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct MeetingStatusView {
    pub session_id: String,
    pub status: MeetingStatus,
    pub url: String,
    /// Per-meeting chat thread id (where the transcript + replies land).
    pub thread: Option<String>,
    /// Human-readable meeting title.
    pub title: Option<String>,
    /// "Mute Magician" state (capture gated; the bot hears nothing).
    pub paused: bool,
    /// Freshest cached summary of the meeting transcript, if any. After teardown
    /// the session runs a final `resummarize()`, so an ended (retained) session
    /// returns its final summary here.
    pub latest_summary: Option<String>,
    /// Whole seconds since the session was registered — the capture duration a
    /// surface shows. Derived from a monotonic clock at read time.
    pub started_seconds_ago: u64,
    /// Whole seconds since teardown returned, or `None` while the session is
    /// still running. See [`MeetingSummaryRow::ended_seconds_ago`].
    pub ended_seconds_ago: Option<u64>,
    /// `(principal, workspace)` the capture was started under, when known.
    pub scope: Option<(String, String)>,
}

#[derive(Default)]
pub struct MeetingSessionManager {
    sessions: Mutex<HashMap<String, Managed>>,
}

static MANAGER: Lazy<Arc<MeetingSessionManager>> =
    Lazy::new(|| Arc::new(MeetingSessionManager::new()));

/// Process-global manager accessor used by the `meeting` compiled tool.
pub fn meeting_manager() -> Arc<MeetingSessionManager> {
    MANAGER.clone()
}

impl MeetingSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Register + spawn a prebuilt session; returns its `session_id`. Seam-agnostic
    /// so it's testable cross-platform (the macOS real-audio builder, `join`, is a
    /// later task). `thread`/`title` are the per-meeting chat-thread metadata
    /// surfaced by `status`/`list` (None for seam-level spawns). `marker` enables
    /// the crash marker: written here, removed when `start()` returns — a marker
    /// that survives the process is an interrupted capture for the boot sweep.
    pub async fn spawn(
        self: &Arc<Self>,
        session: Arc<MeetingSession>,
        url: String,
        thread: Option<String>,
        title: Option<String>,
        marker: Option<super::MarkerContext>,
    ) -> String {
        let session_id = format!("meet-{}", Uuid::new_v4());
        let marker_path = crate::magician_v2::media_seam::meeting_markers::write_marker_for_session(
            marker.as_ref(),
            &session_id,
            "attendee",
            thread.as_deref(),
            title.as_deref(),
        );
        let done = Arc::new(tokio::sync::Notify::new());
        {
            let mut map = self.sessions.lock().await;
            map.insert(
                session_id.clone(),
                Managed {
                    session: session.clone(),
                    url,
                    thread,
                    title,
                    started_at: Instant::now(),
                    // The crash marker is the one thing every scoped spawn
                    // path already carries the scope in, so the registry takes
                    // it from there rather than growing a parallel argument
                    // that a future caller could forget to pass.
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
        let runner = session;
        tokio::spawn(async move {
            runner.start().await;
            // Clean teardown ran (summary + memory write) — clear the marker.
            if let Some(path) = marker_path {
                crate::magician_v2::media_seam::meeting_markers::remove_marker(&path);
            }
            {
                let mut map = mgr.sessions.lock().await;
                if let Some(m) = map.get_mut(&id_for_task) {
                    m.ended_at = Some(Instant::now());
                }
            }
            // Permit semantics: a waiter that calls `notified()` before this fires
            // still wakes, so `leave`'s single waiter is race-free.
            done.notify_one();
        });
        session_id
    }

    pub async fn status(&self, session_id: &str) -> Option<MeetingStatusView> {
        self.reap().await;
        let map = self.sessions.lock().await;
        let m = map.get(session_id)?;
        Some(MeetingStatusView {
            session_id: session_id.to_string(),
            status: m.session.status().await,
            url: m.url.clone(),
            thread: m.thread.clone(),
            title: m.title.clone(),
            paused: m.session.is_paused(),
            latest_summary: m.session.latest_summary().await,
            started_seconds_ago: m.started_at.elapsed().as_secs(),
            ended_seconds_ago: m.ended_at.map(|at| at.elapsed().as_secs()),
            scope: m.scope.clone(),
        })
    }

    /// "Mute Magician": pause/resume a live session's ears. Errors on
    /// unknown/ended ids.
    pub async fn set_paused(&self, session_id: &str, paused: bool) -> Result<(), String> {
        let map = self.sessions.lock().await;
        match map.get(session_id) {
            Some(m) if m.ended_at.is_none() => {
                m.session.set_paused(paused);
                Ok(())
            },
            Some(_) => Err(format!("meeting session already ended: {session_id}")),
            None => Err(format!("unknown meeting session_id: {session_id}")),
        }
    }

    pub async fn list(&self) -> Vec<MeetingSummaryRow> {
        self.reap().await;
        let map = self.sessions.lock().await;
        let mut rows = Vec::with_capacity(map.len());
        for (id, m) in map.iter() {
            rows.push(MeetingSummaryRow {
                session_id: id.clone(),
                status: m.session.status().await,
                url: m.url.clone(),
                thread: m.thread.clone(),
                title: m.title.clone(),
                paused: m.session.is_paused(),
                latest_summary: m.session.latest_summary().await,
                started_seconds_ago: m.started_at.elapsed().as_secs(),
                ended_seconds_ago: m.ended_at.map(|at| at.elapsed().as_secs()),
                scope: m.scope.clone(),
            });
        }
        rows
    }

    /// Freshest rolling summary of the meeting bound to `thread_id`, if one is
    /// registered. Matching on the **thread** rather than the session id is what
    /// confines a room's per-turn context to the meeting it is actually in: a
    /// call can only name its own `ui_thread_id`, so it cannot reach a sibling
    /// meeting by asking for one.
    pub async fn latest_summary_for_thread(&self, thread_id: &str) -> Option<String> {
        let thread_id = thread_id.trim();
        if thread_id.is_empty() {
            return None;
        }
        self.list()
            .await
            .into_iter()
            .find(|row| row.thread.as_deref().map(str::trim) == Some(thread_id))
            .and_then(|row| row.latest_summary)
    }

    /// Stop a session and return its POST-teardown final summary. Fires the cancel
    /// signal, then (if the session hadn't already ended) waits — bounded — for the
    /// spawned `start()` task to finish its final `resummarize()` + memory write
    /// before reading the latest summary, so the caller gets the final summary
    /// rather than the rolling one captured at cancel time.
    pub async fn leave(&self, session_id: &str) -> Result<Option<String>, String> {
        let (session, done, already_ended) = {
            let map = self.sessions.lock().await;
            match map.get(session_id) {
                Some(m) => (m.session.clone(), m.done.clone(), m.ended_at.is_some()),
                None => return Err(format!("unknown meeting session_id: {session_id}")),
            }
        };
        session.stop().await;
        if !already_ended {
            // Bounded wait for teardown (final resummarize + memory write) to finish.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(10), done.notified()).await;
        }
        Ok(session.latest_summary().await)
    }

    async fn reap(&self) {
        let mut map = self.sessions.lock().await;
        map.retain(|_, m| match m.ended_at {
            Some(t) => t.elapsed().as_secs() < ATTENDEE_ENDED_RETAIN_SECS,
            None => true,
        });
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl MeetingSessionManager {
    /// Build the real session (macOS: ScreenCaptureKit + CoreAudio; Linux:
    /// Pulse capture monitor + virtual microphone), then spawn it. Returns
    /// the session id. Mirrors `examples/meet_bot.rs`.
    ///
    /// `scope` is the invoking agent's `(principal, workspace)`; it keys WHERE the
    /// chat-posting lanes (responder + transcript sink) resolve the per-meeting
    /// thread, so a scoped agent's meeting lands in its own chat store. `None`
    /// falls back to the `MEET_BOT_PRINCIPAL`/`MEET_BOT_WORKSPACE` env defaults.
    pub async fn join(
        self: &Arc<Self>,
        config: super::MeetingConfig,
        memory_writer: Arc<dyn super::MeetingMemoryWriter>,
        browser: Arc<dyn super::BrowserJoin>,
        scope: Option<(String, String)>,
        marker: Option<super::MarkerContext>,
        broadcaster: Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
    ) -> Result<String, String> {
        use crate::magician_v2::media_seam::audio::{AudioSink, AudioSource};
        use crate::magician_v2::media_seam::OpenAiTtsProvider;
        use crate::magician_v2::media_seam::{
            default_meeting_responder, default_summarizer_with_telemetry, ChatThreadTranscriptSink,
            MeetingSession, NoopTranscriptSink, Summarizer, TranscriptSink,
        };
        #[cfg(target_os = "macos")]
        use crate::magician_v2::media_seam::{CoreAudioSink, ScreenCaptureAudioSource};
        #[cfg(target_os = "linux")]
        use crate::magician_v2::media_seam::{PulseAudioSink, PulseAudioSource};

        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        if api_key.is_empty() {
            return Err("OPENAI_API_KEY is required for the meeting bot".into());
        }
        let summarizer: Arc<dyn Summarizer> = default_summarizer_with_telemetry(
            broadcaster.clone(),
            scope.clone(),
            Some(format!("meeting:{}", config.meet_url)),
        );
        let (stt, _) = crate::magician_v2::media_seam::resolve_installed_surface_audio_pipeline(
            crate::magician_v2::media_seam::AudioSurface::Meeting,
            scope.clone(),
            config.audio_profile.as_deref(),
            &config.audio_stage_options,
            true,
        )
        .await?;
        #[cfg(target_os = "macos")]
        let (source, sink): (Arc<dyn AudioSource>, Arc<dyn AudioSink>) = {
            let mut source = ScreenCaptureAudioSource::new();
            if let Ok(bundle) = std::env::var("MEET_BOT_TARGET_BUNDLE_ID") {
                source = source.with_bundle_id(bundle);
            }
            (Arc::new(source), Arc::new(CoreAudioSink::new()))
        };
        #[cfg(target_os = "linux")]
        let (source, sink): (Arc<dyn AudioSource>, Arc<dyn AudioSink>) = {
            if let Err(error) =
                crate::magician_v2::media_seam::meeting_bridge_linux::ensure_meeting_devices().await
            {
                return Err(error);
            }
            (
                Arc::new(PulseAudioSource::new()),
                Arc::new(PulseAudioSink::new()),
            )
        };
        // Per-meeting chat thread + display title via the SHARED resolver (the
        // passive listener resolves through the same function, so both rails
        // converge on the same dated thread for the same meeting).
        let resolved = super::resolve_meeting_thread(
            Some(&config.meet_url),
            config.title.as_deref(),
            config.meeting_date.as_deref(),
        );
        let resolved_for_session = resolved.clone();
        let meeting_thread = resolved.thread;
        let session_title = resolved.session_title;
        let announce = format!("Joined {} — live transcript follows.", config.meet_url);
        let thread_meta = meeting_thread.clone();
        let title_meta = session_title.clone();
        let responder = default_meeting_responder(
            api_key.clone(),
            Some(meeting_thread.clone()),
            scope.clone(),
            broadcaster.clone(),
        )
        .await;
        // Live transcript → the same per-meeting thread, display-only (never
        // dispatches the agent). On by default; `MEET_BOT_STREAM_TRANSCRIPT` set to
        // 0/false/off/no disables it. Same thread AND same scope as the responder,
        // so both lanes resolve the SAME chat session.
        let stream_transcript = std::env::var("MEET_BOT_STREAM_TRANSCRIPT")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "off" | "no"
                )
            })
            .unwrap_or(true);
        let transcript_sink: Arc<dyn TranscriptSink> = if stream_transcript {
            Arc::new(ChatThreadTranscriptSink::from_env(
                Some(meeting_thread),
                scope,
                Some(session_title),
                Some(announce),
            ))
        } else {
            Arc::new(NoopTranscriptSink)
        };
        let url = config.meet_url.clone();
        let session = Arc::new(
            MeetingSession::new(config, summarizer)
                .with_stt(stt)
                .with_audio(source, sink)
                .with_responder(responder)
                .with_tts(
                    Arc::new(OpenAiTtsProvider::new(api_key)),
                    std::env::var("MEET_BOT_TTS_VOICE").ok(),
                )
                .with_memory_writer(memory_writer)
                .with_browser_join(browser)
                .with_transcript_sink(transcript_sink)
                .with_resolved_thread(resolved_for_session),
        );
        Ok(self
            .spawn(session, url, Some(thread_meta), Some(title_meta), marker)
            .await)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl MeetingSessionManager {
    pub async fn join(
        self: &Arc<Self>,
        _config: super::MeetingConfig,
        _memory_writer: Arc<dyn super::MeetingMemoryWriter>,
        _browser: Arc<dyn super::BrowserJoin>,
        _scope: Option<(String, String)>,
        _marker: Option<super::MarkerContext>,
        _broadcaster: Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
    ) -> Result<String, String> {
        Err("the meeting bot needs macOS or Linux (ScreenCaptureKit, or PulseAudio)".into())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::magician_v2::media_seam::*;
    use crate::magician_v2::media_seam::{
        audio::{AudioError, AudioSource, NoopAudioSink},
        MeetingConfig, MeetingSession, MeetingStatus, Summarizer, SummarizerError, TranscriptTurn,
    };
    use crate::magician_v2::media_seam::{
        AudioChunk, StreamAudioFormat, StreamingSttEvent, StreamingSttProvider,
        StreamingSttSession, SttError,
    };
    use async_trait::async_trait;
    use tokio::sync::mpsc;

    /// `AudioSource` whose `run()` blocks "forever": it sleeps ~1h so the session
    /// loop only ends if the capture future is dropped (i.e. on cancel). Mirrors
    /// the private double in `session.rs`'s test module.
    struct BlockingSource;

    #[async_trait]
    impl AudioSource for BlockingSource {
        async fn run(&self, _out: mpsc::Sender<AudioChunk>) -> Result<(), AudioError> {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(())
        }
    }

    /// `StreamingSttProvider` whose opened session HOLDS the event sender so the
    /// listen loop never sees a closed stream. The session therefore only ends on
    /// cancel. Mirrors the private double in `session.rs`'s test module.
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

    /// Offline summarizer stub (no Ollama/HTTP); production uses
    /// RouterSummarizer. These lifecycle tests never reach teardown summarize,
    /// so a fixed string is enough.
    struct StubSummarizer;

    #[async_trait]
    impl Summarizer for StubSummarizer {
        async fn summarize(&self, _transcript: &str) -> Result<String, SummarizerError> {
            Ok("stub summary".to_string())
        }
    }

    fn long_lived_session() -> Arc<MeetingSession> {
        Arc::new(
            MeetingSession::new(MeetingConfig::default(), Arc::new(StubSummarizer))
                .with_stt(Arc::new(HoldingStt))
                .with_audio(Arc::new(BlockingSource), Arc::new(NoopAudioSink)),
        )
    }

    #[tokio::test]
    async fn spawn_status_leave_list_lifecycle() {
        let mgr = Arc::new(MeetingSessionManager::new());
        let id = mgr
            .spawn(
                long_lived_session(),
                "https://meet.google.com/test".into(),
                None,
                None,
                None,
            )
            .await;

        let listed = mgr.list().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, id);

        // `spawn` is fire-and-forget: the background `start()` task hasn't
        // necessarily left `Idle` the instant `spawn` returns. Settle on it
        // having started before asserting the running state.
        let st = loop {
            let st = mgr.status(&id).await.expect("status present");
            if st.status != MeetingStatus::Idle {
                break st;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(matches!(
            st.status,
            MeetingStatus::Listening | MeetingStatus::Joining
        ));

        mgr.leave(&id).await.expect("leave ok");

        let mut left = false;
        for _ in 0..20 {
            if let Some(s) = mgr.status(&id).await {
                if s.status == MeetingStatus::Left {
                    left = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(left, "session should reach Left after leave");
    }

    #[tokio::test]
    async fn status_view_exposes_latest_summary_field() {
        let mgr = Arc::new(MeetingSessionManager::new());
        let id = mgr
            .spawn(
                long_lived_session(),
                "https://meet.google.com/t".into(),
                None,
                None,
                None,
            )
            .await;
        let st = mgr.status(&id).await.expect("status present");
        assert!(st.latest_summary.is_none()); // no transcript yet → no summary
        mgr.leave(&id).await.expect("leave ok");
    }

    /// Summarizer returning a caller-chosen string, so two live meetings can be
    /// told apart by the summary the manager hands back.
    struct FixedSummarizer(&'static str);

    #[async_trait]
    impl Summarizer for FixedSummarizer {
        async fn summarize(&self, _transcript: &str) -> Result<String, SummarizerError> {
            Ok(self.0.to_string())
        }
    }

    fn long_lived_session_summarizing(text: &'static str) -> Arc<MeetingSession> {
        Arc::new(
            MeetingSession::new(MeetingConfig::default(), Arc::new(FixedSummarizer(text)))
                .with_stt(Arc::new(HoldingStt))
                .with_audio(Arc::new(BlockingSource), Arc::new(NoopAudioSink)),
        )
    }

    /// A room's per-turn context is confined to its own meeting by the THREAD it
    /// is bound to. Two meetings run at once, each with a distinguishable
    /// summary; asking by thread must return that thread's meeting and never the
    /// sibling's, and an unknown or blank thread must return nothing rather than
    /// falling through to "whichever meeting is running".
    #[tokio::test]
    async fn latest_summary_for_thread_selects_by_thread_and_never_a_sibling_meeting() {
        let mgr = Arc::new(MeetingSessionManager::new());
        let mine = long_lived_session_summarizing("notes from MY meeting");
        let other = long_lived_session_summarizing("notes from ANOTHER meeting");
        let mine_id = mgr
            .spawn(
                mine.clone(),
                "https://meet.google.com/mine".into(),
                Some("meeting-mine-2026-08-18".into()),
                None,
                None,
            )
            .await;
        let other_id = mgr
            .spawn(
                other.clone(),
                "https://meet.google.com/other".into(),
                Some("meeting-other-2026-08-18".into()),
                None,
                None,
            )
            .await;

        for session in [&mine, &other] {
            session
                .record_turn(TranscriptTurn {
                    at_ms: 0,
                    speaker: Some("Guest".into()),
                    text: "hello".into(),
                })
                .await;
            session.resummarize().await;
        }

        assert_eq!(
            mgr.latest_summary_for_thread("meeting-mine-2026-08-18")
                .await
                .as_deref(),
            Some("notes from MY meeting")
        );
        assert_eq!(
            mgr.latest_summary_for_thread("meeting-other-2026-08-18")
                .await
                .as_deref(),
            Some("notes from ANOTHER meeting")
        );
        assert!(mgr
            .latest_summary_for_thread("meeting-nobody-is-in")
            .await
            .is_none());
        assert!(mgr.latest_summary_for_thread("   ").await.is_none());

        mgr.leave(&mine_id).await.expect("leave ok");
        mgr.leave(&other_id).await.expect("leave ok");
    }

    #[tokio::test]
    async fn status_of_unknown_id_is_none() {
        let mgr = Arc::new(MeetingSessionManager::new());
        assert!(mgr.status("nope").await.is_none());
        assert!(mgr.leave("nope").await.is_err());
    }
}
