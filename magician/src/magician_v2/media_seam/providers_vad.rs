use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::magician_v2::media_seam::{AudioChunk, StreamAudioFormat};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VadCapabilities {
    #[serde(default)]
    pub probability_events: bool,
    #[serde(default)]
    pub configurable_threshold: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadSessionConfig {
    pub threshold: f32,
    pub min_speech_ms: u64,
    pub min_silence_ms: u64,
    pub pre_roll_ms: u64,
    pub hangover_ms: u64,
    pub max_utterance_ms: u64,
    #[serde(default)]
    pub gate_only: bool,
}

impl Default for VadSessionConfig {
    fn default() -> Self {
        Self {
            threshold: 0.65,
            min_speech_ms: 250,
            min_silence_ms: 500,
            pre_roll_ms: 400,
            hangover_ms: 600,
            max_utterance_ms: 120_000,
            gate_only: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VadEvent {
    Probability { value: f32, at_ms: u64 },
    SpeechStarted { at_ms: u64 },
    SpeechEnded { at_ms: u64 },
}

#[derive(Debug, thiserror::Error)]
pub enum VadError {
    #[error("VAD provider unavailable: {0}")]
    Unavailable(String),
    #[error("invalid VAD configuration: {0}")]
    InvalidConfig(String),
    #[error("VAD session failed: {0}")]
    Session(String),
}

#[async_trait]
pub trait VadSession: Send + Sync {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), VadError>;
    async fn finish(&self) -> Result<(), VadError>;
}

#[async_trait]
pub trait VadProvider: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> Option<&str> {
        None
    }
    fn default_model(&self) -> &str {
        self.id()
    }
    fn capabilities(&self) -> VadCapabilities {
        VadCapabilities::default()
    }
    async fn open_session(
        &self,
        format: StreamAudioFormat,
        config: VadSessionConfig,
        events: mpsc::Sender<VadEvent>,
    ) -> Result<Box<dyn VadSession>, VadError>;
}

/// Contract adapter for orchestration tests and explicitly disabled profiles.
/// It is never registered by production boot wiring.
pub struct NoopVadProvider;

#[async_trait]
impl VadProvider for NoopVadProvider {
    fn id(&self) -> &str {
        "noop-vad"
    }

    async fn open_session(
        &self,
        _format: StreamAudioFormat,
        _config: VadSessionConfig,
        _events: mpsc::Sender<VadEvent>,
    ) -> Result<Box<dyn VadSession>, VadError> {
        Ok(Box::new(NoopVadSession))
    }
}

struct NoopVadSession;

#[async_trait]
impl VadSession for NoopVadSession {
    async fn push_audio(&self, _chunk: AudioChunk) -> Result<(), VadError> {
        Ok(())
    }

    async fn finish(&self) -> Result<(), VadError> {
        Ok(())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn noop_vad_exercises_the_contract_without_emitting_turns() {
        let (events, mut receiver) = mpsc::channel(1);
        let session = NoopVadProvider
            .open_session(
                StreamAudioFormat::default(),
                VadSessionConfig::default(),
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
