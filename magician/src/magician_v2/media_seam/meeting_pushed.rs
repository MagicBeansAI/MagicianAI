//! Client-pushed audio capture for the passive meeting rail.
//!
//! The macOS listener captures its own audio (ScreenCaptureKit + AVAudioEngine,
//! see `bridge_macos`). A remote client (the iOS in-app mic listener) instead
//! POSTs 16 kHz mono PCM16 chunks to `POST /meetings/{id}/audio`, and those
//! chunks must reach the same [`PassiveMeetingSession`] pipeline. This module is
//! the seam:
//!
//! * [`PushedAudioSource`] implements [`AudioSource`] by draining a bounded
//!   in-memory queue instead of a capture device. It is embedded in a
//!   `capture: "client"` session exactly where `ScreenCaptureAudioSource` sits
//!   in a host session.
//! * [`PushHandle`] is the producer end. The HTTP ingest handler pushes chunks
//!   through it; on a full queue the chunk is **dropped (newest-drop)** and a
//!   counter is bumped — non-blocking BY CONSTRUCTION, mirroring the transcript
//!   sink's discipline (`transcript_sink.rs`). Real-time audio must never block
//!   the request handler to wait for the STT pipeline.
//! * A process-global [ingest registry](register_pushed_ingest) maps
//!   `(session_id, channel)` to the live handle plus the session's owning scope,
//!   so the ingest handler can (a) route a chunk to the right session/channel
//!   and (b) enforce that the caller's scope matches the session's. Every
//!   session teardown path revokes its entries via [`deregister_pushed_session`]
//!   so a subsequent POST gets a definitive "gone" answer.
//!
//! Lifecycle: [`PushedAudioSource::run`] ends (ending the track, which lets the
//! session tear down normally) when either the queue closes (all handles
//! dropped / deregistered) or no chunk arrives within an idle window. A separate
//! longer grace applies before the first chunk, so a session whose client never
//! starts pushing is reclaimed without running the meeting-summary path against
//! silence.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use once_cell::sync::Lazy;
use tokio::sync::{mpsc, Mutex as TokioMutex};
use tracing::warn;

use crate::magician_v2::media_seam::audio::{AudioError, AudioSource};
use crate::magician_v2::media_seam::meeting_passive::{
    PassiveListenerConfig, PassiveMeetingSession,
};
use crate::magician_v2::media_seam::AudioChunk;

/// Max chunks buffered per channel before newest-drop kicks in. A 6 s PCM chunk
/// is the unit; 32 is ~3 min of backlog, far more than the STT pipeline should
/// ever fall behind — if it does, dropping real-time audio is correct.
pub const PUSHED_QUEUE_CAPACITY: usize = 32;

/// No chunk for this long after capture has started → the source ends and the
/// session tears down (reason: client idle). Overridable via
/// `MAGICIAN_MEETING_CLIENT_IDLE_STOP_SECS`.
pub const DEFAULT_CLIENT_IDLE_STOP_SECS: u64 = 120;

/// No first chunk within this long after the session is created (client armed
/// but never started pushing) → reclaim without the meeting-summary path.
pub const DEFAULT_FIRST_CHUNK_GRACE_SECS: u64 = 600;

/// Which pushed track a chunk belongs to. `Primary` is the diarized meeting/room
/// audio (a single room mic must NOT be hard-labelled "You"); `Mic` is the
/// explicit hard-"You" track (idle in v1 but always registered so ingest of it
/// is a benign accept-and-drop rather than a 4xx that would kill the client).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PushChannel {
    Primary,
    Mic,
}

impl PushChannel {
    /// Parse the `channel` query value. Unknown values are rejected so a typo
    /// surfaces rather than silently mis-routing audio.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "primary" => Some(Self::Primary),
            "mic" => Some(Self::Mic),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Mic => "mic",
        }
    }
}

/// Producer end of a [`PushedAudioSource`]'s queue. Cloneable and cheap; the
/// ingest registry holds one per live `(session_id, channel)`.
#[derive(Clone)]
pub struct PushHandle {
    tx: mpsc::Sender<AudioChunk>,
    dropped: Arc<AtomicU64>,
}

impl PushHandle {
    /// Enqueue a chunk. Returns `true` if queued, `false` if dropped (queue full
    /// — newest-drop — or the source has ended). Never blocks.
    pub fn push(&self, chunk: AudioChunk) -> bool {
        match self.tx.try_send(chunk) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            },
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// Total chunks dropped due to a full queue over this handle's lifetime.
    pub fn dropped_total(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Whether the consuming source has ended (queue closed).
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// An [`AudioSource`] fed by pushed chunks rather than a capture device. Held as
/// `Arc<dyn AudioSource>` in a `capture: "client"` session.
pub struct PushedAudioSource {
    rx: TokioMutex<Option<mpsc::Receiver<AudioChunk>>>,
    idle_timeout: Duration,
    first_chunk_grace: Duration,
    channel: PushChannel,
}

impl PushedAudioSource {
    /// Build a source and its producer handle. `capacity` bounds the queue;
    /// `idle_timeout` ends the source after a gap between chunks; the longer
    /// `first_chunk_grace` applies until the first chunk arrives.
    pub fn new(
        channel: PushChannel,
        capacity: usize,
        idle_timeout: Duration,
        first_chunk_grace: Duration,
    ) -> (Self, PushHandle) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let source = Self {
            rx: TokioMutex::new(Some(rx)),
            idle_timeout,
            first_chunk_grace,
            channel,
        };
        (source, PushHandle { tx, dropped })
    }

    /// Convenience constructor using the module defaults + env overrides.
    pub fn with_defaults(channel: PushChannel) -> (Self, PushHandle) {
        let idle = env_secs(
            "MAGICIAN_MEETING_CLIENT_IDLE_STOP_SECS",
            DEFAULT_CLIENT_IDLE_STOP_SECS,
        );
        let grace = env_secs(
            "MAGICIAN_MEETING_CLIENT_FIRST_CHUNK_GRACE_SECS",
            DEFAULT_FIRST_CHUNK_GRACE_SECS,
        );
        Self::new(
            channel,
            PUSHED_QUEUE_CAPACITY,
            Duration::from_secs(idle),
            Duration::from_secs(grace),
        )
    }
}

fn env_secs(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

#[async_trait]
impl AudioSource for PushedAudioSource {
    async fn run(&self, out: mpsc::Sender<AudioChunk>) -> Result<(), AudioError> {
        // `run` is driven once per session; take the receiver out. A second call
        // (should not happen) is a harmless no-op end.
        let mut rx = match self.rx.lock().await.take() {
            Some(rx) => rx,
            None => return Ok(()),
        };
        let mut seen_first = false;
        loop {
            let window = if seen_first {
                self.idle_timeout
            } else {
                self.first_chunk_grace
            };
            tokio::select! {
                maybe = rx.recv() => match maybe {
                    Some(chunk) => {
                        seen_first = true;
                        // Downstream dropped → the session is ending; stop.
                        if out.send(chunk).await.is_err() {
                            return Ok(());
                        }
                    },
                    // All producer handles dropped / deregistered.
                    None => return Ok(()),
                },
                _ = tokio::time::sleep(window) => {
                    warn!(
                        target: "meet_bot",
                        channel = self.channel.as_str(),
                        seen_first,
                        secs = window.as_secs(),
                        "pushed audio source idle timeout; ending capture track"
                    );
                    return Ok(());
                }
            }
        }
    }
}

// --- Process-global ingest registry -----------------------------------------

struct RegisteredIngest {
    handle: PushHandle,
    principal: String,
    workspace: String,
    /// Per-session bearer returned by `POST /meetings/listen`. Possession of the
    /// session id alone must not be enough to inject audio.
    upload_token: String,
}

static PUSHED_INGEST: Lazy<StdMutex<HashMap<(String, PushChannel), RegisteredIngest>>> =
    Lazy::new(|| StdMutex::new(HashMap::new()));

/// Register a channel's producer handle under a live session + its owning scope.
/// Called once per channel when a `capture: "client"` session starts.
pub fn register_pushed_ingest(
    session_id: &str,
    channel: PushChannel,
    handle: PushHandle,
    principal: &str,
    workspace: &str,
    upload_token: &str,
) {
    let mut map = PUSHED_INGEST
        .lock()
        .expect("pushed ingest registry poisoned");
    map.insert(
        (session_id.to_string(), channel),
        RegisteredIngest {
            handle,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            upload_token: upload_token.to_string(),
        },
    );
}

/// Revoke every channel registered for a session. Called from the manager's
/// spawn-completion continuation so EVERY teardown path (stop, idle, silence
/// auto-stop, max-duration, failure) clears ingest; a subsequent POST then gets
/// [`IngestPushResult::Gone`]. Safe/no-op for host (non-client) sessions.
pub fn deregister_pushed_session(session_id: &str) {
    let mut map = PUSHED_INGEST
        .lock()
        .expect("pushed ingest registry poisoned");
    map.retain(|(sid, _), _| sid != session_id);
}

/// Outcome of an ingest attempt. `Gone` deliberately collapses "unknown session"
/// and "scope mismatch" into one answer so the endpoint cannot be used as a
/// cross-scope session-existence oracle.
#[derive(Debug, PartialEq, Eq)]
pub enum IngestPushResult {
    /// Chunk delivered (or newest-dropped under backpressure). `dropped_total` is
    /// the session-channel's cumulative drop count for observability.
    Accepted { dropped: u64 },
    /// No such live session/channel for this scope — terminal for the client.
    Gone,
}

/// Route one chunk to a live `(session_id, channel)` after verifying the caller's
/// scope AND per-session upload token match the session's. Non-blocking
/// (newest-drop under backpressure).
pub fn push_ingest_chunk(
    session_id: &str,
    channel: PushChannel,
    principal: &str,
    workspace: &str,
    upload_token: &str,
    chunk: AudioChunk,
) -> IngestPushResult {
    let map = PUSHED_INGEST
        .lock()
        .expect("pushed ingest registry poisoned");
    match map.get(&(session_id.to_string(), channel)) {
        Some(entry)
            if entry.principal == principal
                && entry.workspace == workspace
                && entry.upload_token == upload_token =>
        {
            entry.handle.push(chunk);
            IngestPushResult::Accepted {
                dropped: entry.handle.dropped_total(),
            }
        },
        // Unknown session/channel OR wrong scope OR bad token — uniform, no oracle.
        _ => IngestPushResult::Gone,
    }
}

// --- Client capture-mode session start (v1) ---------------------------------

/// Outcome of starting (or reusing) a `capture: "client"` listener.
pub struct ClientListenResult {
    pub session_id: String,
    /// Per-session bearer the client must echo on every `POST .../audio`.
    pub upload_token: String,
    /// True when an already-live client session on this thread was returned.
    pub reused: bool,
}

/// Why a client listener could not start.
pub enum StartClientListenerError {
    /// A HOST (Mac-captured) listener is already live on this meeting thread —
    /// two capture sources on one thread double-post every line. The client
    /// should surface "already being observed from the Mac" (HTTP 409).
    Conflict { existing_session_id: String },
    /// Pipeline/resolution failure (HTTP 500).
    Internal(String),
}

struct ClientSessionRecord {
    session_id: String,
    upload_token: String,
}

/// `thread -> live client session`, for capture-mode-aware idempotency. A live
/// session found by the manager that is NOT here is a host session (→ conflict).
static CLIENT_SESSIONS: Lazy<StdMutex<HashMap<String, ClientSessionRecord>>> =
    Lazy::new(|| StdMutex::new(HashMap::new()));

/// Pure client-session constructor: a [`PassiveMeetingSession`] fed by
/// [`PushedAudioSource`]s (no capture device) with injected services. Returns
/// the session plus the producer handles to register. Testable without the
/// boot-installed audio pipeline. `Primary` carries the diarized room/meeting
/// audio; `Mic` is the hard-"You" track (consumed only when `capture_mic`, but
/// its handle is always returned so ingest of it is accept-and-drop, never 4xx).
pub fn build_client_passive_session(
    config: PassiveListenerConfig,
    summarizer: std::sync::Arc<dyn crate::magician_v2::media_seam::meeting_summarizer::Summarizer>,
    stt: std::sync::Arc<dyn crate::magician_v2::media_seam::StreamingSttProvider>,
    mic_stt: std::sync::Arc<dyn crate::magician_v2::media_seam::StreamingSttProvider>,
    sink: std::sync::Arc<
        dyn crate::magician_v2::media_seam::meeting_transcript_sink::TranscriptSink,
    >,
    memory_writer: std::sync::Arc<dyn super::MeetingMemoryWriter>,
) -> (
    std::sync::Arc<PassiveMeetingSession>,
    Vec<(PushChannel, PushHandle)>,
) {
    let capture_mic = config.capture_mic;
    let (primary_source, primary_handle) = PushedAudioSource::with_defaults(PushChannel::Primary);
    let (mic_source_impl, mic_handle) = PushedAudioSource::with_defaults(PushChannel::Mic);
    let system_source: Arc<dyn AudioSource> = Arc::new(primary_source);
    let mic_source: Option<Arc<dyn AudioSource>> = if capture_mic {
        Some(Arc::new(mic_source_impl))
    } else {
        // Not consumed by the session; its registered handle then accept-and-drops.
        drop(mic_source_impl);
        None
    };
    let session = Arc::new(
        PassiveMeetingSession::new(config, summarizer, stt, system_source, mic_source)
            .with_mic_stt(mic_stt)
            .with_transcript_sink(sink)
            .with_memory_writer(memory_writer),
    );
    (
        session,
        vec![
            (PushChannel::Primary, primary_handle),
            (PushChannel::Mic, mic_handle),
        ],
    )
}

/// Start (or idempotently reuse) a `capture: "client"` passive listener. Mirrors
/// the macOS [`start_passive_listener`](super::meeting_passive::start_passive_listener)
/// but the capture sources are pushed queues fed by `POST /meetings/{id}/audio`.
/// Not `#[cfg]`-gated: client capture is platform-agnostic.
pub async fn start_client_passive_listener(
    config: PassiveListenerConfig,
    memory_writer: std::sync::Arc<dyn super::MeetingMemoryWriter>,
    scope: Option<(String, String)>,
    marker: Option<super::MarkerContext>,
    broadcaster: Option<
        std::sync::Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
    >,
) -> Result<ClientListenResult, StartClientListenerError> {
    use crate::magician_v2::media_seam::summarizer::{
        default_summarizer_with_telemetry, Summarizer,
    };
    use crate::magician_v2::media_seam::transcript_sink::{
        ChatThreadTranscriptSink, TranscriptSink,
    };

    let mgr = super::meeting_passive::passive_meeting_manager();

    // Capture-mode-aware idempotency: a live session on this thread is either
    // ours (client → reuse) or a host session (→ conflict).
    if let Some(existing) = mgr.find_live_by_thread(&config.thread).await {
        let ours = {
            let map = CLIENT_SESSIONS.lock().expect("client sessions poisoned");
            map.get(&config.thread)
                .filter(|rec| rec.session_id == existing)
                .map(|rec| rec.upload_token.clone())
        };
        return match ours {
            Some(upload_token) => Ok(ClientListenResult {
                session_id: existing,
                upload_token,
                reused: true,
            }),
            None => Err(StartClientListenerError::Conflict {
                existing_session_id: existing,
            }),
        };
    }

    let (principal, workspace) = scope
        .clone()
        .unwrap_or_else(|| ("anonymous".to_string(), "default".to_string()));

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
    .await
    .map_err(StartClientListenerError::Internal)?;
    let (mic_stt, _) = crate::magician_v2::media_seam::resolve_installed_surface_audio_pipeline(
        crate::magician_v2::media_seam::AudioSurface::Meeting,
        scope.clone(),
        config.audio_profile.as_deref(),
        &config.audio_stage_options,
        false,
    )
    .await
    .map_err(StartClientListenerError::Internal)?;
    let announce = match &config.url {
        Some(url) => format!("Listening to this meeting from your phone: {url}"),
        None => "Listening to this meeting from your phone.".to_string(),
    };
    let sink: Arc<dyn TranscriptSink> = Arc::new(ChatThreadTranscriptSink::from_env(
        Some(config.thread.clone()),
        scope.clone(),
        Some(config.session_title.clone()),
        Some(announce),
    ));

    let thread = config.thread.clone();
    let (session, handles) =
        build_client_passive_session(config, summarizer, stt, mic_stt, sink, memory_writer);
    let session_id = mgr.spawn(session, marker).await;

    let upload_token = format!("ut-{}", uuid::Uuid::new_v4());
    for (channel, handle) in handles {
        register_pushed_ingest(
            &session_id,
            channel,
            handle,
            &principal,
            &workspace,
            &upload_token,
        );
    }
    CLIENT_SESSIONS
        .lock()
        .expect("client sessions poisoned")
        .insert(
            thread,
            ClientSessionRecord {
                session_id: session_id.clone(),
                upload_token: upload_token.clone(),
            },
        );

    Ok(ClientListenResult {
        session_id,
        upload_token,
        reused: false,
    })
}

/// Called from the manager's spawn-completion path when ANY passive session
/// ends. No-op for host sessions; for client sessions it revokes ingest and
/// clears the idempotency record so a re-listen starts fresh and a late POST
/// gets [`IngestPushResult::Gone`].
pub fn on_session_ended(session_id: &str) {
    deregister_pushed_session(session_id);
    CLIENT_SESSIONS
        .lock()
        .expect("client sessions poisoned")
        .retain(|_, rec| rec.session_id != session_id);
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::*;
    use bytes::Bytes;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn chunk(seq: u64) -> AudioChunk {
        AudioChunk {
            seq,
            pcm: Bytes::from(vec![0u8; 16]),
        }
    }

    #[tokio::test]
    async fn pushed_source_forwards_chunks_in_order() {
        let (source, handle) = PushedAudioSource::new(
            PushChannel::Primary,
            8,
            Duration::from_millis(200),
            Duration::from_millis(200),
        );
        let (out_tx, mut out_rx) = mpsc::channel(8);
        let run = tokio::spawn(async move { source.run(out_tx).await });

        assert!(handle.push(chunk(0)));
        assert!(handle.push(chunk(1)));
        assert!(handle.push(chunk(2)));

        for expected in 0..3u64 {
            let got = out_rx.recv().await.expect("chunk delivered");
            assert_eq!(got.seq, expected, "chunks delivered in push order");
        }
        assert_eq!(handle.dropped_total(), 0, "nothing dropped under capacity");
        // Dropping the handle closes the queue → run ends.
        drop(handle);
        let _ = run.await.expect("run task joined");
    }

    #[tokio::test]
    async fn pushed_source_drops_newest_when_full() {
        // Capacity 2 and NO consumer running yet → the 3rd push overflows.
        let (source, handle) = PushedAudioSource::new(
            PushChannel::Primary,
            2,
            Duration::from_millis(200),
            Duration::from_millis(200),
        );
        assert!(handle.push(chunk(0)));
        assert!(handle.push(chunk(1)));
        // Queue full → newest dropped, counter bumped, queued chunks untouched.
        assert!(!handle.push(chunk(2)));
        assert_eq!(handle.dropped_total(), 1);

        // Now drain and confirm the two queued (oldest) chunks survived.
        let (out_tx, mut out_rx) = mpsc::channel(8);
        let run = tokio::spawn(async move { source.run(out_tx).await });
        assert_eq!(out_rx.recv().await.unwrap().seq, 0);
        assert_eq!(out_rx.recv().await.unwrap().seq, 1);
        drop(handle);
        let _ = run.await;
    }

    #[tokio::test]
    async fn pushed_source_run_ends_on_deregister() {
        let (source, handle) = PushedAudioSource::new(
            PushChannel::Primary,
            8,
            Duration::from_secs(30),
            Duration::from_secs(30),
        );
        let (out_tx, _out_rx) = mpsc::channel(8);
        let run = tokio::spawn(async move { source.run(out_tx).await });
        // Dropping the only handle (what deregister does to registry-held handles)
        // closes the queue; run must return promptly, not wait out the 30s idle.
        drop(handle);
        let joined = tokio::time::timeout(Duration::from_secs(5), run).await;
        assert!(joined.is_ok(), "run ended when the queue closed");
    }

    #[tokio::test(start_paused = true)]
    async fn pushed_source_idle_timeout_ends_run() {
        // never_started: the first-chunk grace elapses with no chunk → run ends.
        // Awaited inline (NOT spawned): tokio's paused clock auto-advances to the
        // pending grace timer once the task is otherwise idle, so `run` returns
        // without any real waiting. Both `_handle` (queue open) and `_out_rx`
        // (downstream open) are held, so the grace timer is the ONLY exit.
        let (source, _handle) = PushedAudioSource::new(
            PushChannel::Primary,
            8,
            Duration::from_secs(120),
            Duration::from_secs(600),
        );
        let (out_tx, _out_rx) = mpsc::channel(8);
        source
            .run(out_tx)
            .await
            .expect("run returns after the first-chunk grace");
    }

    #[test]
    fn ingest_registry_routes_and_scope_guards() {
        let sid = "listen-test-1";
        let (_source, handle) = PushedAudioSource::new(
            PushChannel::Primary,
            8,
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        register_pushed_ingest(sid, PushChannel::Primary, handle, "p", "w", "tok");

        // Right scope + token → accepted.
        assert!(matches!(
            push_ingest_chunk(sid, PushChannel::Primary, "p", "w", "tok", chunk(0)),
            IngestPushResult::Accepted { .. }
        ));
        // Wrong scope → Gone (no oracle).
        assert_eq!(
            push_ingest_chunk(sid, PushChannel::Primary, "other", "w", "tok", chunk(1)),
            IngestPushResult::Gone
        );
        // Wrong token → Gone.
        assert_eq!(
            push_ingest_chunk(sid, PushChannel::Primary, "p", "w", "nope", chunk(1)),
            IngestPushResult::Gone
        );
        // Unregistered channel → Gone.
        assert_eq!(
            push_ingest_chunk(sid, PushChannel::Mic, "p", "w", "tok", chunk(2)),
            IngestPushResult::Gone
        );
        // Deregister → subsequent push is Gone.
        deregister_pushed_session(sid);
        assert_eq!(
            push_ingest_chunk(sid, PushChannel::Primary, "p", "w", "tok", chunk(3)),
            IngestPushResult::Gone
        );
    }
}
