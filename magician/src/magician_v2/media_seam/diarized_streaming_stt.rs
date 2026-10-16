use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex};

use super::providers::{
    AudioChunk, DiarizationEvent, DiarizationProvider, DiarizationSession,
    DiarizationSessionConfig, SpeakerSegment, StreamAudioFormat, StreamingSttCapabilities,
    StreamingSttEvent, StreamingSttProvider, StreamingSttSession, SttError,
};

const EVENT_CAPACITY: usize = 64;
const ATTRIBUTION_WAIT: Duration = Duration::from_millis(1_500);
const MAX_TIMELINE_SEGMENTS: usize = 4_096;
const TIMELINE_RETENTION_MS: u64 = 10 * 60 * 1_000;

pub struct DiarizedStreamingSttProvider {
    inner: Arc<dyn StreamingSttProvider>,
    diarization: Vec<Arc<dyn DiarizationProvider>>,
}

impl DiarizedStreamingSttProvider {
    pub fn new(
        inner: Arc<dyn StreamingSttProvider>,
        diarization: Vec<Arc<dyn DiarizationProvider>>,
    ) -> Self {
        Self { inner, diarization }
    }
}

#[async_trait]
impl StreamingSttProvider for DiarizedStreamingSttProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn label(&self) -> Option<&str> {
        self.inner.label()
    }

    fn default_model(&self) -> &str {
        self.inner.default_model()
    }

    fn capabilities(&self) -> StreamingSttCapabilities {
        StreamingSttCapabilities {
            speaker_attribution: true,
            ..self.inner.capabilities()
        }
    }

    async fn open_session(
        &self,
        format: StreamAudioFormat,
        events: mpsc::Sender<StreamingSttEvent>,
    ) -> Result<Box<dyn StreamingSttSession>, SttError> {
        let (stt_events, stt_receiver) = mpsc::channel(EVENT_CAPACITY);
        let stt = self.inner.open_session(format, stt_events).await?;
        let (diarization_events, diarization_receiver) = mpsc::channel(EVENT_CAPACITY);
        let mut opened = None;
        for provider in &self.diarization {
            match provider
                .open_session(
                    format,
                    DiarizationSessionConfig::default(),
                    diarization_events.clone(),
                )
                .await
            {
                Ok(session) => {
                    opened = Some(session);
                    break;
                },
                Err(error) => tracing::warn!(
                    provider = provider.id(),
                    %error,
                    "diarization provider failed to open; trying the next provider"
                ),
            }
        }
        let Some(diarization) = opened else {
            spawn_passthrough(stt_receiver, events);
            return Ok(stt);
        };
        tokio::spawn(join_timelines(stt_receiver, diarization_receiver, events));
        Ok(Box::new(DiarizedStreamingSttSession {
            stt,
            diarization: Mutex::new(Some(diarization)),
        }))
    }
}

struct DiarizedStreamingSttSession {
    stt: Box<dyn StreamingSttSession>,
    diarization: Mutex<Option<Box<dyn DiarizationSession>>>,
}

#[async_trait]
impl StreamingSttSession for DiarizedStreamingSttSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
        self.stt.push_audio(chunk.clone()).await?;
        let mut diarization = self.diarization.lock().await;
        if let Some(session) = diarization.as_ref() {
            if let Err(error) = session.push_audio(chunk).await {
                tracing::warn!(%error, "diarization failed mid-session; continuing without speaker labels");
                *diarization = None;
            }
        }
        Ok(())
    }

    async fn finish(&self) -> Result<(), SttError> {
        let stt_result = self.stt.finish().await;
        if let Some(session) = self.diarization.lock().await.take() {
            if let Err(error) = session.finish().await {
                tracing::warn!(%error, "diarization finish failed; transcript remains available");
            }
        }
        stt_result
    }
}

fn spawn_passthrough(
    mut receiver: mpsc::Receiver<StreamingSttEvent>,
    events: mpsc::Sender<StreamingSttEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            if events.send(event).await.is_err() {
                break;
            }
        }
    });
}

struct PendingFinal {
    event: StreamingSttEvent,
    received_at: Instant,
}

async fn join_timelines(
    mut stt_events: mpsc::Receiver<StreamingSttEvent>,
    mut diarization_events: mpsc::Receiver<DiarizationEvent>,
    events: mpsc::Sender<StreamingSttEvent>,
) {
    let mut segments = Vec::<SpeakerSegment>::new();
    let mut active = BTreeSet::<String>::new();
    let mut pending = Vec::<PendingFinal>::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(200));
    let mut stt_open = true;
    let mut diarization_open = true;

    while stt_open || !pending.is_empty() {
        tokio::select! {
            event = stt_events.recv(), if stt_open => match event {
                Some(event @ StreamingSttEvent::Final { speaker: Some(_), .. }) => {
                    let _ = events.send(event).await;
                },
                Some(event @ StreamingSttEvent::Final { .. }) => {
                    if let Some(attributed) = attribute(event.clone(), &segments, &active) {
                        let _ = events.send(attributed).await;
                    } else if diarization_open {
                        pending.push(PendingFinal { event, received_at: Instant::now() });
                    } else {
                        let _ = events.send(event).await;
                    }
                },
                Some(StreamingSttEvent::Partial { text, speaker }) => {
                    let speaker = speaker.or_else(|| single_active(&active));
                    let _ = events.send(StreamingSttEvent::Partial { text, speaker }).await;
                },
                Some(event) => { let _ = events.send(event).await; },
                None => stt_open = false,
            },
            event = diarization_events.recv(), if diarization_open => match event {
                Some(DiarizationEvent::SpeakerStarted { speaker_id, .. }) => {
                    active.insert(speaker_id);
                },
                Some(DiarizationEvent::SpeakerEnded { speaker_id, .. }) => {
                    active.remove(&speaker_id);
                },
                Some(DiarizationEvent::SegmentRevised { segment }) => {
                    if let Some(existing) = segments.iter_mut().find(|item| {
                        item.speaker_id == segment.speaker_id && item.start_ms == segment.start_ms
                    }) {
                        *existing = segment;
                    } else {
                        segments.push(segment);
                    }
                    prune_segments(&mut segments);
                    flush_pending(&mut pending, &segments, &active, &events, false).await;
                },
                None => {
                    diarization_open = false;
                    flush_pending(&mut pending, &segments, &active, &events, true).await;
                },
            },
            _ = ticker.tick() => {
                flush_pending(&mut pending, &segments, &active, &events, !diarization_open).await;
            },
        }
    }
}

fn prune_segments(segments: &mut Vec<SpeakerSegment>) {
    let newest_end = segments
        .iter()
        .map(|segment| segment.end_ms)
        .max()
        .unwrap_or(0);
    let cutoff = newest_end.saturating_sub(TIMELINE_RETENTION_MS);
    segments.retain(|segment| segment.end_ms >= cutoff);
    if segments.len() > MAX_TIMELINE_SEGMENTS {
        segments.sort_by_key(|segment| segment.end_ms);
        segments.drain(..segments.len() - MAX_TIMELINE_SEGMENTS);
    }
}

async fn flush_pending(
    pending: &mut Vec<PendingFinal>,
    segments: &[SpeakerSegment],
    active: &BTreeSet<String>,
    events: &mpsc::Sender<StreamingSttEvent>,
    force: bool,
) {
    let now = Instant::now();
    let mut keep = Vec::new();
    for item in pending.drain(..) {
        if let Some(attributed) = attribute(item.event.clone(), segments, active) {
            let _ = events.send(attributed).await;
        } else if force || now.duration_since(item.received_at) >= ATTRIBUTION_WAIT {
            let _ = events.send(item.event).await;
        } else {
            keep.push(item);
        }
    }
    *pending = keep;
}

fn attribute(
    event: StreamingSttEvent,
    segments: &[SpeakerSegment],
    active: &BTreeSet<String>,
) -> Option<StreamingSttEvent> {
    let (text, speaker, language, start_ms) = match event {
        StreamingSttEvent::Final {
            text,
            speaker,
            language,
            start_ms,
        } => (text, speaker, language, start_ms),
        event => return Some(event),
    };
    if speaker.is_some() {
        return Some(StreamingSttEvent::Final {
            text,
            speaker,
            language,
            start_ms,
        });
    }
    let at_ms = start_ms.unwrap_or(0);
    let selected = segments
        .iter()
        .filter(|segment| segment.start_ms <= at_ms && at_ms <= segment.end_ms)
        .max_by(|left, right| {
            left.confidence
                .unwrap_or_default()
                .total_cmp(&right.confidence.unwrap_or_default())
        })
        .map(|segment| segment.speaker_id.clone())
        .or_else(|| single_active(active));
    selected.map(|speaker| StreamingSttEvent::Final {
        text,
        speaker: Some(speaker),
        language,
        start_ms,
    })
}

fn single_active(active: &BTreeSet<String>) -> Option<String> {
    (active.len() == 1)
        .then(|| active.iter().next().cloned())
        .flatten()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::{attribute, SpeakerSegment};
    use crate::magician_v2::media_seam::StreamingSttEvent;
    use std::collections::BTreeSet;

    #[test]
    fn final_is_attributed_from_covering_segment() {
        let event = StreamingSttEvent::Final {
            text: "hello".to_string(),
            speaker: None,
            language: Some("en".to_string()),
            start_ms: Some(500),
        };
        let attributed = attribute(
            event,
            &[SpeakerSegment {
                speaker_id: "speaker_2".to_string(),
                start_ms: 200,
                end_ms: 900,
                confidence: Some(0.8),
            }],
            &BTreeSet::new(),
        )
        .expect("attributed");
        assert!(matches!(
            attributed,
            StreamingSttEvent::Final { speaker: Some(value), .. } if value == "speaker_2"
        ));
    }
}
