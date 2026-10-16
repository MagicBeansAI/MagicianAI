//! On-device macOS Speech streaming-STT adapter — the meeting bot's low-latency
//! "ears".
//!
//! Spawns the `magician-macos-meet-audio` helper in `--mode transcribe`: PCM16
//! pushed to its stdin is transcribed on-device by `SFSpeechRecognizer`, which
//! emits JSON transcript events (`{"type":"partial"|"final","text":"…"}`) on
//! stdout. We forward [`push_audio`](StreamingSttSession::push_audio) PCM to the
//! helper and surface its finals as [`StreamingSttEvent::Final`] — near-real-time,
//! free, no network (vs [`super::openai_streaming_stt`]'s ~Ns cloud segments).
//!
//! Needs the **Speech Recognition** TCC grant (the transcribe mode does not
//! capture, so it does NOT need Screen Recording — that's the capture source's
//! grant).

use std::process::Stdio;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use super::streaming_stt::{
    AudioChunk, StreamAudioFormat, StreamingSttCapabilities, StreamingSttEvent,
    StreamingSttProvider, StreamingSttSession,
};
use super::stt::SttError;

pub const MACOS_SPEECH_STT_PROVIDER_ID: &str = "macos-speech";
pub const MACOS_SPEECH_STT_HELPER_ENV: &str = "MAGICIAN_MACOS_MEET_AUDIO_BIN";
pub const MACOS_SPEECH_STT_DEFAULT_HELPER: &str = "./magician-macos-meet-audio.bin";
const HELPER_FINISH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);

fn helper_path() -> String {
    std::env::var(MACOS_SPEECH_STT_HELPER_ENV)
        .unwrap_or_else(|_| MACOS_SPEECH_STT_DEFAULT_HELPER.to_string())
}

/// Opens on-device transcription sessions backed by the macOS Speech helper.
#[derive(Clone)]
pub struct MacOsSpeechSttProvider {
    helper_path: String,
    locale: String,
    contextual: Vec<String>,
}

impl Default for MacOsSpeechSttProvider {
    fn default() -> Self {
        Self {
            helper_path: helper_path(),
            locale: "en-US".to_string(),
            // Bias recognition toward the shipped name and the in-lexicon
            // spellings used by constrained on-device wake spotters.
            contextual: vec![
                "Hey Magican".to_string(),
                "Hey magical".to_string(),
                "Hey magician".to_string(),
                "Magican".to_string(),
                "magical".to_string(),
                "magician".to_string(),
            ],
        }
    }
}

impl MacOsSpeechSttProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_helper_path(mut self, path: impl Into<String>) -> Self {
        self.helper_path = path.into();
        self
    }

    pub fn with_locale(mut self, locale: impl Into<String>) -> Self {
        self.locale = locale.into();
        self
    }

    /// Phrases to bias recognition toward (the wake words). Empty disables.
    pub fn with_contextual(mut self, phrases: Vec<String>) -> Self {
        self.contextual = phrases;
        self
    }
}

#[derive(Deserialize)]
struct HelperTranscript {
    #[serde(rename = "type")]
    kind: String,
    text: String,
}

#[async_trait]
impl StreamingSttProvider for MacOsSpeechSttProvider {
    fn id(&self) -> &str {
        MACOS_SPEECH_STT_PROVIDER_ID
    }

    fn label(&self) -> Option<&str> {
        Some("macOS Speech")
    }

    fn default_model(&self) -> &str {
        "system_default"
    }

    fn capabilities(&self) -> StreamingSttCapabilities {
        StreamingSttCapabilities {
            partial_results: true,
            ..StreamingSttCapabilities::default()
        }
    }

    async fn open_session(
        &self,
        format: StreamAudioFormat,
        events: mpsc::Sender<StreamingSttEvent>,
    ) -> Result<Box<dyn StreamingSttSession>, SttError> {
        let mut child = Command::new(&self.helper_path)
            .arg("--mode")
            .arg("transcribe")
            .arg("--sample-rate")
            .arg(format.sample_rate_hz.to_string())
            .arg("--channels")
            .arg(format.channels.to_string())
            .arg("--locale")
            .arg(&self.locale)
            .arg("--contextual")
            .arg(self.contextual.join(","))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| SttError::Transport(format!("spawn {}: {e}", self.helper_path)))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SttError::Transport("transcribe helper has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SttError::Transport("transcribe helper has no stdout".into()))?;

        // Diagnostics (ready / errors) on stderr → logs.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "meet_bot", helper = "meet-audio-transcribe", "{line}");
                }
            });
        }

        // Transcript events on stdout → StreamingSttEvent.
        let event_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(t) = serde_json::from_str::<HelperTranscript>(line) else {
                    continue; // non-JSON line — ignore
                };
                let event = if t.kind == "final" {
                    StreamingSttEvent::Final {
                        text: t.text,
                        speaker: None,
                        language: None,
                        start_ms: None,
                    }
                } else {
                    StreamingSttEvent::Partial {
                        text: t.text,
                        speaker: None,
                    }
                };
                if events.send(event).await.is_err() {
                    break; // consumer gone
                }
            }
        });

        Ok(Box::new(MacOsSpeechSession {
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(child),
            event_task: Mutex::new(Some(event_task)),
        }))
    }
}

struct MacOsSpeechSession {
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Child>,
    event_task: Mutex<Option<JoinHandle<()>>>,
}

#[async_trait]
impl StreamingSttSession for MacOsSpeechSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
        let mut guard = self.stdin.lock().await;
        match guard.as_mut() {
            Some(stdin) => stdin
                .write_all(&chunk.pcm)
                .await
                .map_err(|e| SttError::Transport(format!("transcribe helper stdin: {e}"))),
            None => Err(SttError::Transport(
                "transcribe session already finished".into(),
            )),
        }
    }

    async fn finish(&self) -> Result<(), SttError> {
        // Drop stdin → helper hits EOF → flushes its final + exits.
        self.stdin.lock().await.take();
        let status = {
            let mut child = self.child.lock().await;
            match tokio::time::timeout(HELPER_FINISH_TIMEOUT, child.wait()).await {
                Ok(result) => result.map_err(|error| {
                    SttError::Transport(format!("waiting for transcribe helper: {error}"))
                })?,
                Err(_) => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return Err(SttError::Transport(
                        "transcribe helper timed out while flushing final transcript".into(),
                    ));
                },
            }
        };
        if let Some(task) = self.event_task.lock().await.take() {
            task.await.map_err(|error| {
                SttError::Transport(format!("joining transcribe helper event stream: {error}"))
            })?;
        }
        if !status.success() {
            return Err(SttError::Transport(format!(
                "transcribe helper exited with {status}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg(unix)]
    async fn finish_waits_for_helper_eof_flush_before_exit() {
        let marker = std::env::temp_dir().join(format!(
            "magician-macos-stt-finish-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("cat >/dev/null; printf flushed > \"$1\"")
            .arg("macos-stt-test")
            .arg(&marker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn helper fixture");
        let stdin = child.stdin.take().expect("fixture stdin");
        let session = MacOsSpeechSession {
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(child),
            event_task: Mutex::new(Some(tokio::spawn(async {}))),
        };

        session
            .push_audio(AudioChunk {
                seq: 1,
                pcm: bytes::Bytes::from_static(&[1, 2]),
            })
            .await
            .expect("write audio");
        session.finish().await.expect("graceful finish");
        assert_eq!(
            std::fs::read_to_string(&marker).expect("flush marker"),
            "flushed"
        );
        let _ = std::fs::remove_file(marker);
    }
}
