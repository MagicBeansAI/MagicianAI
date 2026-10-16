use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::magician_v2::media_seam::{AudioChunk, StreamAudioFormat};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiarizationCapabilities {
    #[serde(default)]
    pub online_revisions: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_speakers: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiarizationSessionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_speakers: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerSegment {
    pub speaker_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DiarizationEvent {
    SpeakerStarted { speaker_id: String, at_ms: u64 },
    SpeakerEnded { speaker_id: String, at_ms: u64 },
    SegmentRevised { segment: SpeakerSegment },
}

#[derive(Debug, thiserror::Error)]
pub enum DiarizationError {
    #[error("diarization provider unavailable: {0}")]
    Unavailable(String),
    #[error("invalid diarization configuration: {0}")]
    InvalidConfig(String),
    #[error("diarization session failed: {0}")]
    Session(String),
}

#[async_trait]
pub trait DiarizationSession: Send + Sync {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), DiarizationError>;
    async fn finish(&self) -> Result<(), DiarizationError>;
}

#[async_trait]
pub trait DiarizationProvider: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> Option<&str> {
        None
    }
    fn default_model(&self) -> &str {
        self.id()
    }
    fn capabilities(&self) -> DiarizationCapabilities {
        DiarizationCapabilities::default()
    }
    async fn open_session(
        &self,
        format: StreamAudioFormat,
        config: DiarizationSessionConfig,
        events: mpsc::Sender<DiarizationEvent>,
    ) -> Result<Box<dyn DiarizationSession>, DiarizationError>;
}

/// Contract adapter for orchestration tests and explicitly disabled profiles.
/// It is never registered by production boot wiring.
pub struct NoopDiarizationProvider;

#[async_trait]
impl DiarizationProvider for NoopDiarizationProvider {
    fn id(&self) -> &str {
        "noop-diarization"
    }

    async fn open_session(
        &self,
        _format: StreamAudioFormat,
        _config: DiarizationSessionConfig,
        _events: mpsc::Sender<DiarizationEvent>,
    ) -> Result<Box<dyn DiarizationSession>, DiarizationError> {
        Ok(Box::new(NoopDiarizationSession))
    }
}

struct NoopDiarizationSession;

#[async_trait]
impl DiarizationSession for NoopDiarizationSession {
    async fn push_audio(&self, _chunk: AudioChunk) -> Result<(), DiarizationError> {
        Ok(())
    }

    async fn finish(&self) -> Result<(), DiarizationError> {
        Ok(())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn noop_diarization_exercises_the_contract_without_emitting_segments() {
        let (events, mut receiver) = mpsc::channel(1);
        let session = NoopDiarizationProvider
            .open_session(
                StreamAudioFormat::default(),
                DiarizationSessionConfig::default(),
                events,
            )
            .await
            .expect("session");
        session
            .push_audio(AudioChunk {
                seq: 1,
                pcm: bytes::Bytes::new(),
            })
            .await
            .expect("push");
        session.finish().await.expect("finish");
        assert!(receiver.try_recv().is_err());
    }
}
