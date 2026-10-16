use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};

use super::providers::{
    AudioChunk, StreamAudioFormat, StreamingSttCapabilities, StreamingSttEvent,
    StreamingSttProvider, StreamingSttSession, SttError,
};

const EVENT_CAPACITY: usize = 64;
const MAX_REPLAY_MS: u64 = 30_000;
const MAX_COMMITTED_KEYS: usize = 128;
const REPLAY_DEDUPE_GRACE: Duration = Duration::from_secs(10);
const POST_FINISH_SUPERVISOR_GRACE: Duration = Duration::from_secs(5);

pub struct FallbackStreamingSttProvider {
    providers: Vec<Arc<dyn StreamingSttProvider>>,
    transition_observer: Option<StreamingSttFallbackObserver>,
}

impl FallbackStreamingSttProvider {
    pub fn new(providers: Vec<Arc<dyn StreamingSttProvider>>) -> Self {
        Self {
            providers,
            transition_observer: None,
        }
    }

    pub fn with_observer(
        providers: Vec<Arc<dyn StreamingSttProvider>>,
        transition_observer: StreamingSttFallbackObserver,
    ) -> Self {
        Self {
            providers,
            transition_observer: Some(transition_observer),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingSttFallbackState {
    ProviderFailed,
    ProviderActivated,
    Exhausted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingSttFallbackTransition {
    pub session_id: String,
    pub state: StreamingSttFallbackState,
    pub from_provider: Option<String>,
    pub to_provider: Option<String>,
    pub generation: u64,
    pub replayed_chunks: usize,
    pub error_class: Option<String>,
}

pub type StreamingSttFallbackObserver =
    Arc<dyn Fn(StreamingSttFallbackTransition) + Send + Sync + 'static>;

enum ForwardControl {
    Drain(oneshot::Sender<()>),
}

#[async_trait]
impl StreamingSttProvider for FallbackStreamingSttProvider {
    fn id(&self) -> &str {
        self.providers
            .first()
            .map_or("streaming-fallback", |item| item.id())
    }

    fn label(&self) -> Option<&str> {
        self.providers.first().and_then(|item| item.label())
    }

    fn default_model(&self) -> &str {
        self.providers
            .first()
            .map_or("streaming-fallback", |item| item.default_model())
    }

    fn capabilities(&self) -> StreamingSttCapabilities {
        self.providers
            .first()
            .map_or_else(StreamingSttCapabilities::default, |item| {
                item.capabilities()
            })
    }

    async fn open_session(
        &self,
        format: StreamAudioFormat,
        events: mpsc::Sender<StreamingSttEvent>,
    ) -> Result<Box<dyn StreamingSttSession>, SttError> {
        let session_id = uuid::Uuid::new_v4().simple().to_string();
        let replay = Arc::new(Mutex::new(VecDeque::new()));
        let committed = Arc::new(Mutex::new(CommittedFinals::default()));
        let active_generation = Arc::new(AtomicU64::new(0));
        let mut failures = Vec::new();
        for (index, provider) in self.providers.iter().enumerate() {
            let (provider_events, receiver) = mpsc::channel(EVENT_CAPACITY);
            match provider.open_session(format, provider_events).await {
                Ok(session) => {
                    let generation = 1;
                    active_generation.store(generation, Ordering::Release);
                    if index > 0 {
                        notify_transition(
                            &self.transition_observer,
                            StreamingSttFallbackTransition {
                                session_id: session_id.clone(),
                                state: StreamingSttFallbackState::ProviderActivated,
                                from_provider: self
                                    .providers
                                    .get(index.saturating_sub(1))
                                    .map(|provider| provider.id().to_string()),
                                to_provider: Some(provider.id().to_string()),
                                generation,
                                replayed_chunks: 0,
                                error_class: None,
                            },
                        );
                    }
                    let inner = Arc::new(FallbackStreamingSttInner {
                        session_id,
                        format,
                        events,
                        providers: self.providers.clone(),
                        transition_observer: self.transition_observer.clone(),
                        state: Mutex::new(FallbackState {
                            index,
                            generation,
                            session: Arc::from(session),
                            forward_control: None,
                            finish_requested: false,
                        }),
                        replay,
                        committed,
                        active_generation,
                    });
                    let control = spawn_event_forwarder(receiver, &inner, generation);
                    inner.state.lock().await.forward_control = Some(control);
                    return Ok(Box::new(FallbackStreamingSttSession { inner }));
                },
                Err(error) => {
                    notify_transition(
                        &self.transition_observer,
                        StreamingSttFallbackTransition {
                            session_id: session_id.clone(),
                            state: StreamingSttFallbackState::ProviderFailed,
                            from_provider: Some(provider.id().to_string()),
                            to_provider: self
                                .providers
                                .get(index + 1)
                                .map(|next| next.id().to_string()),
                            generation: 0,
                            replayed_chunks: 0,
                            error_class: Some(stt_error_class(&error)),
                        },
                    );
                    failures.push(format!("{}: {error}", provider.id()));
                },
            }
        }
        notify_transition(
            &self.transition_observer,
            StreamingSttFallbackTransition {
                session_id,
                state: StreamingSttFallbackState::Exhausted,
                from_provider: self
                    .providers
                    .last()
                    .map(|provider| provider.id().to_string()),
                to_provider: None,
                generation: 0,
                replayed_chunks: 0,
                error_class: Some("open_failed".to_string()),
            },
        );
        Err(SttError::NotConfigured(format!(
            "all streaming STT providers failed to open: {}",
            failures.join("; ")
        )))
    }
}

struct FallbackState {
    index: usize,
    generation: u64,
    session: Arc<dyn StreamingSttSession>,
    forward_control: Option<mpsc::Sender<ForwardControl>>,
    finish_requested: bool,
}

struct ReplayChunk {
    chunk: AudioChunk,
    duration_ms: u64,
}

struct FallbackStreamingSttSession {
    inner: Arc<FallbackStreamingSttInner>,
}

struct FallbackStreamingSttInner {
    session_id: String,
    format: StreamAudioFormat,
    events: mpsc::Sender<StreamingSttEvent>,
    providers: Vec<Arc<dyn StreamingSttProvider>>,
    transition_observer: Option<StreamingSttFallbackObserver>,
    state: Mutex<FallbackState>,
    replay: Arc<Mutex<VecDeque<ReplayChunk>>>,
    committed: Arc<Mutex<CommittedFinals>>,
    active_generation: Arc<AtomicU64>,
}

#[async_trait]
impl StreamingSttSession for FallbackStreamingSttSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
        let mut state = self.inner.state.lock().await;
        append_replay(&self.inner.replay, self.inner.format, chunk.clone()).await;
        match state.session.push_audio(chunk).await {
            Ok(()) => Ok(()),
            Err(error) => self.inner.rotate(&mut state, error).await,
        }
    }

    async fn finish(&self) -> Result<(), SttError> {
        let (session, initial_generation, initial_control) = {
            let mut state = self.inner.state.lock().await;
            state.finish_requested = true;
            (
                Arc::clone(&state.session),
                state.generation,
                state.forward_control.clone(),
            )
        };
        if let Err(error) = session.finish().await {
            let mut state = self.inner.state.lock().await;
            if state.generation == initial_generation {
                self.inner.rotate(&mut state, error).await?;
            }
        }

        let initial_drain = match initial_control {
            Some(control) => drain_forwarder(&control).await,
            None => Err(SttError::Transport(
                "streaming fallback event forwarder was not installed".into(),
            )),
        };
        // An asynchronous provider error may have rotated while `finish()` was
        // flushing. The replacement is already finished by `rotate` when
        // `finish_requested` is set; drain its final events as well.
        let replacement_control = {
            let state = self.inner.state.lock().await;
            (state.generation != initial_generation)
                .then(|| state.forward_control.clone())
                .flatten()
        };
        if let Some(control) = replacement_control {
            drain_forwarder(&control).await?;
        } else {
            initial_drain?;
        }

        {
            let inner = Arc::clone(&self.inner);
            tokio::spawn(async move {
                tokio::time::sleep(POST_FINISH_SUPERVISOR_GRACE).await;
                drop(inner);
            });
        }
        Ok(())
    }
}

impl FallbackStreamingSttInner {
    async fn rotate_if_current(self: &Arc<Self>, generation: u64, reason: String) {
        let result = {
            let mut state = self.state.lock().await;
            if state.generation != generation {
                return;
            }
            self.rotate(&mut state, SttError::Transport(reason)).await
        };
        if let Err(error) = result {
            let _ = self
                .events
                .send(StreamingSttEvent::Error {
                    reason: error.to_string(),
                })
                .await;
        }
    }

    async fn rotate(
        self: &Arc<Self>,
        state: &mut FallbackState,
        initial_error: SttError,
    ) -> Result<(), SttError> {
        let failed_provider = self.providers[state.index].id().to_string();
        self.active_generation.store(0, Ordering::Release);
        warn!(
            target: "magician::metrics::streaming_stt_fallback",
            from_provider = self.providers[state.index].id(),
            generation = state.generation,
            error = %initial_error,
            "streaming STT provider failed; attempting configured fallback"
        );
        if !state.finish_requested {
            let _ = state.session.finish().await;
        }
        notify_transition(
            &self.transition_observer,
            StreamingSttFallbackTransition {
                session_id: self.session_id.clone(),
                state: StreamingSttFallbackState::ProviderFailed,
                from_provider: Some(failed_provider.clone()),
                to_provider: self
                    .providers
                    .get(state.index + 1)
                    .map(|provider| provider.id().to_string()),
                generation: state.generation,
                replayed_chunks: 0,
                error_class: Some(stt_error_class(&initial_error)),
            },
        );
        let mut failures = vec![format!(
            "{}: {initial_error}",
            self.providers[state.index].id()
        )];
        let mut last_failed_provider = failed_provider;
        let replay = self
            .replay
            .lock()
            .await
            .iter()
            .map(|item| item.chunk.clone())
            .collect::<Vec<_>>();
        self.committed.lock().await.begin_replay();
        for index in (state.index + 1)..self.providers.len() {
            let provider = &self.providers[index];
            let (provider_events, receiver) = mpsc::channel(EVENT_CAPACITY);
            state.generation = state.generation.saturating_add(1);
            let generation = state.generation;
            match provider.open_session(self.format, provider_events).await {
                Ok(session) => {
                    self.active_generation.store(generation, Ordering::Release);
                    let forward_control = spawn_event_forwarder(receiver, self, generation);
                    let mut replay_error = None;
                    for chunk in &replay {
                        if let Err(error) = session.push_audio(chunk.clone()).await {
                            replay_error = Some(error);
                            break;
                        }
                    }
                    if let Some(error) = replay_error {
                        self.active_generation.store(0, Ordering::Release);
                        failures.push(format!("{} replay: {error}", provider.id()));
                        notify_transition(
                            &self.transition_observer,
                            StreamingSttFallbackTransition {
                                session_id: self.session_id.clone(),
                                state: StreamingSttFallbackState::ProviderFailed,
                                from_provider: Some(provider.id().to_string()),
                                to_provider: self
                                    .providers
                                    .get(index + 1)
                                    .map(|next| next.id().to_string()),
                                generation,
                                replayed_chunks: replay.len(),
                                error_class: Some(stt_error_class(&error)),
                            },
                        );
                        last_failed_provider = provider.id().to_string();
                        let _ = session.finish().await;
                        continue;
                    }
                    if state.finish_requested {
                        if let Err(error) = session.finish().await {
                            self.active_generation.store(0, Ordering::Release);
                            failures.push(format!("{} finish: {error}", provider.id()));
                            notify_transition(
                                &self.transition_observer,
                                StreamingSttFallbackTransition {
                                    session_id: self.session_id.clone(),
                                    state: StreamingSttFallbackState::ProviderFailed,
                                    from_provider: Some(provider.id().to_string()),
                                    to_provider: self
                                        .providers
                                        .get(index + 1)
                                        .map(|next| next.id().to_string()),
                                    generation,
                                    replayed_chunks: replay.len(),
                                    error_class: Some(stt_error_class(&error)),
                                },
                            );
                            last_failed_provider = provider.id().to_string();
                            continue;
                        }
                    }
                    state.index = index;
                    state.session = Arc::from(session);
                    state.forward_control = Some(forward_control);
                    notify_transition(
                        &self.transition_observer,
                        StreamingSttFallbackTransition {
                            session_id: self.session_id.clone(),
                            state: StreamingSttFallbackState::ProviderActivated,
                            from_provider: Some(last_failed_provider),
                            to_provider: Some(provider.id().to_string()),
                            generation,
                            replayed_chunks: replay.len(),
                            error_class: None,
                        },
                    );
                    info!(
                        target: "magician::metrics::streaming_stt_fallback",
                        to_provider = provider.id(),
                        generation,
                        replayed_chunks = replay.len(),
                        "streaming STT fallback activated"
                    );
                    return Ok(());
                },
                Err(error) => {
                    failures.push(format!("{}: {error}", provider.id()));
                    notify_transition(
                        &self.transition_observer,
                        StreamingSttFallbackTransition {
                            session_id: self.session_id.clone(),
                            state: StreamingSttFallbackState::ProviderFailed,
                            from_provider: Some(provider.id().to_string()),
                            to_provider: self
                                .providers
                                .get(index + 1)
                                .map(|next| next.id().to_string()),
                            generation,
                            replayed_chunks: replay.len(),
                            error_class: Some(stt_error_class(&error)),
                        },
                    );
                    last_failed_provider = provider.id().to_string();
                },
            }
        }
        warn!(
            target: "magician::metrics::streaming_stt_fallback",
            attempted_providers = self.providers.len(),
            "streaming STT fallback chain exhausted"
        );
        notify_transition(
            &self.transition_observer,
            StreamingSttFallbackTransition {
                session_id: self.session_id.clone(),
                state: StreamingSttFallbackState::Exhausted,
                from_provider: Some(last_failed_provider),
                to_provider: None,
                generation: state.generation,
                replayed_chunks: replay.len(),
                error_class: Some("fallback_exhausted".to_string()),
            },
        );
        Err(SttError::NotConfigured(format!(
            "streaming STT failed and no fallback opened: {}",
            failures.join("; ")
        )))
    }
}

fn notify_transition(
    observer: &Option<StreamingSttFallbackObserver>,
    transition: StreamingSttFallbackTransition,
) {
    if let Some(observer) = observer {
        observer(transition);
    }
}

fn stt_error_class(error: &SttError) -> String {
    match error {
        SttError::NotConfigured(_) => "not_configured",
        SttError::BadRequest(_) => "bad_request",
        SttError::Upstream { .. } => "upstream",
        SttError::Transport(_) => "transport",
        SttError::NoSpeech => "no_speech",
    }
    .to_string()
}

async fn append_replay(
    replay: &Mutex<VecDeque<ReplayChunk>>,
    format: StreamAudioFormat,
    chunk: AudioChunk,
) {
    let bytes_per_sample = match format.sample_format {
        super::providers::StreamSampleFormat::PcmS16Le => 2,
        super::providers::StreamSampleFormat::PcmF32Le => 4,
    };
    let frames = chunk.pcm.len() as u64 / (bytes_per_sample * usize::from(format.channels)) as u64;
    let duration_ms = frames.saturating_mul(1_000) / u64::from(format.sample_rate_hz.max(1));
    let mut replay = replay.lock().await;
    replay.push_back(ReplayChunk { chunk, duration_ms });
    let mut total = replay.iter().map(|item| item.duration_ms).sum::<u64>();
    while total > MAX_REPLAY_MS {
        let Some(removed) = replay.pop_front() else {
            break;
        };
        total = total.saturating_sub(removed.duration_ms);
    }
}

#[derive(Default)]
struct CommittedFinals {
    order: VecDeque<String>,
    keys: HashSet<String>,
    replay_until: Option<Instant>,
}

impl CommittedFinals {
    fn begin_replay(&mut self) {
        self.replay_until = Some(Instant::now() + REPLAY_DEDUPE_GRACE);
    }

    fn insert(&mut self, text: &str) -> bool {
        let key = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        if key.is_empty() {
            return false;
        }
        let replaying = self
            .replay_until
            .is_some_and(|deadline| Instant::now() <= deadline);
        if replaying && self.keys.contains(&key) {
            self.replay_until = None;
            return false;
        }
        if replaying {
            self.replay_until = None;
        }
        if self.keys.contains(&key) {
            return true;
        }
        self.keys.insert(key.clone());
        self.order.push_back(key);
        while self.order.len() > MAX_COMMITTED_KEYS {
            if let Some(removed) = self.order.pop_front() {
                self.keys.remove(&removed);
            }
        }
        true
    }
}

fn spawn_event_forwarder(
    mut receiver: mpsc::Receiver<StreamingSttEvent>,
    inner: &Arc<FallbackStreamingSttInner>,
    generation: u64,
) -> mpsc::Sender<ForwardControl> {
    let inner = Arc::downgrade(inner);
    let (control_tx, mut control_rx) = mpsc::channel(1);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                control = control_rx.recv() => {
                    let Some(ForwardControl::Drain(ack)) = control else { break };
                    while let Ok(event) = receiver.try_recv() {
                        if !forward_fallback_event(event, &inner, generation).await {
                            break;
                        }
                    }
                    let _ = ack.send(());
                    break;
                },
                event = receiver.recv() => {
                    let Some(event) = event else { break };
                    if !forward_fallback_event(event, &inner, generation).await {
                        break;
                    }
                }
            }
        }
    });
    control_tx
}

async fn drain_forwarder(control: &mpsc::Sender<ForwardControl>) -> Result<(), SttError> {
    let (ack_tx, ack_rx) = oneshot::channel();
    // A closed/gone forwarder at drain time is BENIGN, not fatal: the forwarder
    // task exits when its event receiver closes, which means every event it held
    // was already forwarded — there is nothing left to drain. Failing here used to
    // fail the whole finish/commit and degrade the entire local transcript on a
    // teardown race (common right after a provider failover, e.g. parakeet ->
    // macos-speech). Treat both the send failure (forwarder gone) and a dropped
    // ack (forwarder exited mid-drain) as "fully drained".
    if control.send(ForwardControl::Drain(ack_tx)).await.is_err() {
        debug!(
            target: "magician::metrics::streaming_stt_fallback",
            "streaming fallback event forwarder already closed at drain; treating as fully drained"
        );
        return Ok(());
    }
    if ack_rx.await.is_err() {
        debug!(
            target: "magician::metrics::streaming_stt_fallback",
            "streaming fallback event drain ack dropped; forwarder gone, treating as drained"
        );
    }
    Ok(())
}

async fn forward_fallback_event(
    event: StreamingSttEvent,
    inner: &std::sync::Weak<FallbackStreamingSttInner>,
    generation: u64,
) -> bool {
    let Some(inner) = inner.upgrade() else {
        return false;
    };
    if inner.active_generation.load(Ordering::Acquire) != generation {
        debug!(
            target: "magician::metrics::streaming_stt_fallback",
            generation,
            "suppressed stale streaming STT provider event"
        );
        return true;
    }
    if let StreamingSttEvent::Error { reason } = &event {
        inner.rotate_if_current(generation, reason.clone()).await;
        return false;
    }
    if let StreamingSttEvent::Final { text, .. } = &event {
        if !inner.committed.lock().await.insert(text) {
            debug!(
                target: "magician::metrics::streaming_stt_fallback",
                generation,
                "suppressed duplicate replayed streaming STT final"
            );
            return true;
        }
        inner.replay.lock().await.clear();
    }
    inner.events.send(event).await.is_ok()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use bytes::Bytes;
    use tokio::sync::{mpsc, Mutex};

    use super::{CommittedFinals, FallbackStreamingSttProvider, StreamingSttFallbackState};
    use crate::magician_v2::media_seam::{
        AudioChunk, StreamAudioFormat, StreamSampleFormat, StreamingSttCapabilities,
        StreamingSttEvent, StreamingSttProvider, StreamingSttSession, SttError,
    };

    #[test]
    fn committed_final_dedupe_ignores_case_and_whitespace() {
        let mut committed = CommittedFinals::default();
        assert!(committed.insert(" Card  due tomorrow "));
        assert!(committed.insert("card due tomorrow"));
        committed.begin_replay();
        assert!(!committed.insert("card due tomorrow"));
        assert!(committed.insert("card due tomorrow"));
        assert!(committed.insert("another turn"));
    }

    struct AsyncFaultProvider {
        id: &'static str,
        emit_fault_on_push: bool,
        emit_fault_on_finish: bool,
        fail_open: bool,
        fail_push: bool,
        fail_finish: bool,
        opens: Arc<AtomicUsize>,
        received: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    #[async_trait]
    impl StreamingSttProvider for AsyncFaultProvider {
        fn id(&self) -> &str {
            self.id
        }

        fn capabilities(&self) -> StreamingSttCapabilities {
            StreamingSttCapabilities {
                partial_results: true,
                end_of_utterance: true,
                ..StreamingSttCapabilities::default()
            }
        }

        async fn open_session(
            &self,
            _format: StreamAudioFormat,
            events: mpsc::Sender<StreamingSttEvent>,
        ) -> Result<Box<dyn StreamingSttSession>, SttError> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            if self.fail_open {
                return Err(SttError::Transport(format!("{} open failure", self.id)));
            }
            Ok(Box::new(AsyncFaultSession {
                id: self.id,
                emit_fault_on_push: self.emit_fault_on_push,
                emit_fault_on_finish: self.emit_fault_on_finish,
                fail_push: self.fail_push,
                fail_finish: self.fail_finish,
                fault_sent: Mutex::new(false),
                received: Arc::clone(&self.received),
                events,
            }))
        }
    }

    struct AsyncFaultSession {
        id: &'static str,
        emit_fault_on_push: bool,
        emit_fault_on_finish: bool,
        fail_push: bool,
        fail_finish: bool,
        fault_sent: Mutex<bool>,
        received: Arc<Mutex<Vec<Vec<u8>>>>,
        events: mpsc::Sender<StreamingSttEvent>,
    }

    #[async_trait]
    impl StreamingSttSession for AsyncFaultSession {
        async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
            self.received.lock().await.push(chunk.pcm.to_vec());
            if self.fail_push {
                return Err(SttError::Transport(format!("{} push failure", self.id)));
            }
            let mut fault_sent = self.fault_sent.lock().await;
            if self.emit_fault_on_push && !*fault_sent {
                *fault_sent = true;
                let _ = self
                    .events
                    .send(StreamingSttEvent::Error {
                        reason: format!("{} asynchronous failure", self.id),
                    })
                    .await;
            } else {
                let _ = self
                    .events
                    .send(StreamingSttEvent::Partial {
                        text: format!("{} partial", self.id),
                        speaker: None,
                    })
                    .await;
            }
            Ok(())
        }

        async fn finish(&self) -> Result<(), SttError> {
            if self.fail_finish {
                return Err(SttError::Transport(format!("{} finish failure", self.id)));
            }
            if self.emit_fault_on_finish {
                let _ = self
                    .events
                    .send(StreamingSttEvent::Error {
                        reason: format!("{} asynchronous finish failure", self.id),
                    })
                    .await;
            } else if !self.emit_fault_on_push {
                let _ = self
                    .events
                    .send(StreamingSttEvent::Final {
                        text: format!("{} final", self.id),
                        speaker: None,
                        language: Some("en".to_string()),
                        start_ms: None,
                    })
                    .await;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn asynchronous_provider_error_rotates_and_replays_without_leaking_error() {
        let primary_received = Arc::new(Mutex::new(Vec::new()));
        let fallback_received = Arc::new(Mutex::new(Vec::new()));
        let primary_opens = Arc::new(AtomicUsize::new(0));
        let fallback_opens = Arc::new(AtomicUsize::new(0));
        let provider = FallbackStreamingSttProvider::new(vec![
            Arc::new(AsyncFaultProvider {
                id: "primary",
                emit_fault_on_push: true,
                emit_fault_on_finish: false,
                fail_open: false,
                fail_push: false,
                fail_finish: false,
                opens: Arc::clone(&primary_opens),
                received: Arc::clone(&primary_received),
            }),
            Arc::new(AsyncFaultProvider {
                id: "fallback",
                emit_fault_on_push: false,
                emit_fault_on_finish: false,
                fail_open: false,
                fail_push: false,
                fail_finish: false,
                opens: Arc::clone(&fallback_opens),
                received: Arc::clone(&fallback_received),
            }),
        ]);
        let (events_tx, mut events_rx) = mpsc::channel(16);
        let session = provider
            .open_session(
                StreamAudioFormat {
                    sample_rate_hz: 24_000,
                    channels: 1,
                    sample_format: StreamSampleFormat::PcmS16Le,
                },
                events_tx,
            )
            .await
            .expect("open fallback chain");

        session
            .push_audio(AudioChunk {
                seq: 1,
                pcm: Bytes::from_static(&[1, 2]),
            })
            .await
            .expect("primary accepts before asynchronous fault");
        tokio::time::sleep(Duration::from_millis(10)).await;
        session
            .push_audio(AudioChunk {
                seq: 2,
                pcm: Bytes::from_static(&[3, 4]),
            })
            .await
            .expect("fallback accepts replay and current audio");
        session.finish().await.expect("finish fallback");

        let mut observed = Vec::new();
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(50), events_rx.recv()).await
        {
            observed.push(event);
        }
        assert!(observed
            .iter()
            .all(|event| !matches!(event, StreamingSttEvent::Error { .. })));
        assert!(observed.iter().any(|event| matches!(
            event,
            StreamingSttEvent::Final { text, .. } if text == "fallback final"
        )));
        assert_eq!(primary_opens.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_opens.load(Ordering::SeqCst), 1);
        assert_eq!(*primary_received.lock().await, vec![vec![1, 2]]);
        assert_eq!(
            *fallback_received.lock().await,
            vec![vec![1, 2], vec![3, 4]]
        );
    }

    #[tokio::test]
    async fn asynchronous_finish_error_rotates_replays_and_finishes_fallback() {
        let primary_received = Arc::new(Mutex::new(Vec::new()));
        let fallback_received = Arc::new(Mutex::new(Vec::new()));
        let fallback_opens = Arc::new(AtomicUsize::new(0));
        let provider = FallbackStreamingSttProvider::new(vec![
            Arc::new(AsyncFaultProvider {
                id: "primary",
                emit_fault_on_push: false,
                emit_fault_on_finish: true,
                fail_open: false,
                fail_push: false,
                fail_finish: false,
                opens: Arc::new(AtomicUsize::new(0)),
                received: Arc::clone(&primary_received),
            }),
            Arc::new(AsyncFaultProvider {
                id: "fallback",
                emit_fault_on_push: false,
                emit_fault_on_finish: false,
                fail_open: false,
                fail_push: false,
                fail_finish: false,
                opens: Arc::clone(&fallback_opens),
                received: Arc::clone(&fallback_received),
            }),
        ]);
        let (events_tx, mut events_rx) = mpsc::channel(16);
        let session = provider
            .open_session(
                StreamAudioFormat {
                    sample_rate_hz: 24_000,
                    channels: 1,
                    sample_format: StreamSampleFormat::PcmS16Le,
                },
                events_tx,
            )
            .await
            .expect("open fallback chain");

        session
            .push_audio(AudioChunk {
                seq: 1,
                pcm: Bytes::from_static(&[1, 2, 3, 4]),
            })
            .await
            .expect("primary audio");
        session.finish().await.expect("primary finish accepted");

        let mut observed = Vec::new();
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(250), events_rx.recv()).await
        {
            let complete = matches!(
                &event,
                StreamingSttEvent::Final { text, .. } if text == "fallback final"
            );
            observed.push(event);
            if complete {
                break;
            }
        }
        assert!(observed
            .iter()
            .all(|event| !matches!(event, StreamingSttEvent::Error { .. })));
        assert!(observed.iter().any(|event| matches!(
            event,
            StreamingSttEvent::Final { text, .. } if text == "fallback final"
        )));
        assert_eq!(fallback_opens.load(Ordering::SeqCst), 1);
        assert_eq!(*primary_received.lock().await, vec![vec![1, 2, 3, 4]]);
        assert_eq!(*fallback_received.lock().await, vec![vec![1, 2, 3, 4]]);
    }

    #[tokio::test]
    async fn synchronous_push_failure_rotates_and_replays_current_chunk() {
        let fallback_received = Arc::new(Mutex::new(Vec::new()));
        let provider = FallbackStreamingSttProvider::new(vec![
            Arc::new(AsyncFaultProvider {
                id: "primary",
                emit_fault_on_push: false,
                emit_fault_on_finish: false,
                fail_open: false,
                fail_push: true,
                fail_finish: false,
                opens: Arc::new(AtomicUsize::new(0)),
                received: Arc::new(Mutex::new(Vec::new())),
            }),
            Arc::new(AsyncFaultProvider {
                id: "fallback",
                emit_fault_on_push: false,
                emit_fault_on_finish: false,
                fail_open: false,
                fail_push: false,
                fail_finish: false,
                opens: Arc::new(AtomicUsize::new(0)),
                received: Arc::clone(&fallback_received),
            }),
        ]);
        let (events_tx, _events_rx) = mpsc::channel(16);
        let session = provider
            .open_session(
                StreamAudioFormat {
                    sample_rate_hz: 24_000,
                    channels: 1,
                    sample_format: StreamSampleFormat::PcmS16Le,
                },
                events_tx,
            )
            .await
            .expect("open fallback chain");

        session
            .push_audio(AudioChunk {
                seq: 1,
                pcm: Bytes::from_static(&[7, 8]),
            })
            .await
            .expect("synchronous failure rotates");
        assert_eq!(*fallback_received.lock().await, vec![vec![7, 8]]);
    }

    #[tokio::test]
    async fn finish_does_not_return_before_final_crosses_fallback_forwarder() {
        let provider = FallbackStreamingSttProvider::new(vec![Arc::new(AsyncFaultProvider {
            id: "primary",
            emit_fault_on_push: false,
            emit_fault_on_finish: false,
            fail_open: false,
            fail_push: false,
            fail_finish: false,
            opens: Arc::new(AtomicUsize::new(0)),
            received: Arc::new(Mutex::new(Vec::new())),
        })]);
        let (events_tx, mut events_rx) = mpsc::channel(16);
        let session = provider
            .open_session(StreamAudioFormat::default(), events_tx)
            .await
            .expect("open fallback wrapper");

        session.finish().await.expect("finish with drain barrier");
        assert!(matches!(
            events_rx.try_recv(),
            Ok(StreamingSttEvent::Final { text, .. }) if text == "primary final"
        ));
    }

    #[tokio::test]
    async fn open_failure_uses_next_configured_provider() {
        let fallback_received = Arc::new(Mutex::new(Vec::new()));
        let fallback_opens = Arc::new(AtomicUsize::new(0));
        let transitions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = Arc::clone(&transitions);
        let provider = FallbackStreamingSttProvider::with_observer(
            vec![
                Arc::new(AsyncFaultProvider {
                    id: "unavailable",
                    emit_fault_on_push: false,
                    emit_fault_on_finish: false,
                    fail_open: true,
                    fail_push: false,
                    fail_finish: false,
                    opens: Arc::new(AtomicUsize::new(0)),
                    received: Arc::new(Mutex::new(Vec::new())),
                }),
                Arc::new(AsyncFaultProvider {
                    id: "fallback",
                    emit_fault_on_push: false,
                    emit_fault_on_finish: false,
                    fail_open: false,
                    fail_push: false,
                    fail_finish: false,
                    opens: Arc::clone(&fallback_opens),
                    received: Arc::clone(&fallback_received),
                }),
            ],
            Arc::new(move |transition| {
                observed.lock().expect("transition lock").push(transition);
            }),
        );
        let (events_tx, _events_rx) = mpsc::channel(16);
        let session = provider
            .open_session(
                StreamAudioFormat {
                    sample_rate_hz: 24_000,
                    channels: 1,
                    sample_format: StreamSampleFormat::PcmS16Le,
                },
                events_tx,
            )
            .await
            .expect("fallback opens");
        session
            .push_audio(AudioChunk {
                seq: 1,
                pcm: Bytes::from_static(&[9, 10]),
            })
            .await
            .expect("fallback receives audio");

        assert_eq!(fallback_opens.load(Ordering::SeqCst), 1);
        assert_eq!(*fallback_received.lock().await, vec![vec![9, 10]]);
        let transitions = transitions.lock().expect("transition lock");
        assert_eq!(transitions.len(), 2);
        assert_eq!(
            transitions[0].state,
            StreamingSttFallbackState::ProviderFailed
        );
        assert_eq!(transitions[0].from_provider.as_deref(), Some("unavailable"));
        assert_eq!(
            transitions[1].state,
            StreamingSttFallbackState::ProviderActivated
        );
        assert_eq!(transitions[1].to_provider.as_deref(), Some("fallback"));
        assert_eq!(transitions[0].session_id, transitions[1].session_id);
    }

    #[tokio::test]
    async fn runtime_rotation_reports_each_failed_intermediate_provider() {
        let transitions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = Arc::clone(&transitions);
        let final_received = Arc::new(Mutex::new(Vec::new()));
        let provider = FallbackStreamingSttProvider::with_observer(
            vec![
                Arc::new(AsyncFaultProvider {
                    id: "primary",
                    emit_fault_on_push: false,
                    emit_fault_on_finish: false,
                    fail_open: false,
                    fail_push: true,
                    fail_finish: false,
                    opens: Arc::new(AtomicUsize::new(0)),
                    received: Arc::new(Mutex::new(Vec::new())),
                }),
                Arc::new(AsyncFaultProvider {
                    id: "middle",
                    emit_fault_on_push: false,
                    emit_fault_on_finish: false,
                    fail_open: true,
                    fail_push: false,
                    fail_finish: false,
                    opens: Arc::new(AtomicUsize::new(0)),
                    received: Arc::new(Mutex::new(Vec::new())),
                }),
                Arc::new(AsyncFaultProvider {
                    id: "final",
                    emit_fault_on_push: false,
                    emit_fault_on_finish: false,
                    fail_open: false,
                    fail_push: false,
                    fail_finish: false,
                    opens: Arc::new(AtomicUsize::new(0)),
                    received: Arc::clone(&final_received),
                }),
            ],
            Arc::new(move |transition| {
                observed.lock().expect("transition lock").push(transition);
            }),
        );
        let (events_tx, _events_rx) = mpsc::channel(16);
        let session = provider
            .open_session(StreamAudioFormat::default(), events_tx)
            .await
            .expect("primary opens");

        session
            .push_audio(AudioChunk {
                seq: 1,
                pcm: Bytes::from_static(&[4, 5]),
            })
            .await
            .expect("final fallback receives replay");

        assert_eq!(*final_received.lock().await, vec![vec![4, 5]]);
        let transitions = transitions.lock().expect("transition lock");
        assert_eq!(transitions.len(), 3);
        assert_eq!(transitions[0].from_provider.as_deref(), Some("primary"));
        assert_eq!(transitions[0].error_class.as_deref(), Some("transport"));
        assert_eq!(transitions[1].from_provider.as_deref(), Some("middle"));
        assert_eq!(transitions[1].error_class.as_deref(), Some("transport"));
        assert_eq!(
            transitions[2].state,
            StreamingSttFallbackState::ProviderActivated
        );
        assert_eq!(transitions[2].from_provider.as_deref(), Some("middle"));
        assert_eq!(transitions[2].to_provider.as_deref(), Some("final"));
    }
}
