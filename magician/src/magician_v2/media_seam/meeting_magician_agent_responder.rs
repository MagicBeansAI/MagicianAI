//! Meeting responder that routes the query to the magician agent ("Presto").
//!
//! The bot is only an audio surface — the THINKING is the magician runtime's
//! agent (full capabilities, tools, chat/task context, persona), NOT a
//! standalone LLM. On a wake phrase this sends the meeting context + the
//! participant's utterance to the agent via the running server's chat API
//! (`POST /chat/sessions/{id}/messages`, `voice_origin: true`), takes the
//! agent's `speech_segments` as the spoken reply, and synthesizes it with OpenAI
//! TTS for the sink. Gemma stays summary-only.
//!
//! Config (env): `MEET_BOT_MAGICIAN_URL` (default the local server),
//! `MEET_BOT_PRINCIPAL` / `MEET_BOT_WORKSPACE` (default `anonymous` / `default`),
//! `MEET_BOT_BEARER_TOKEN` (scoped credential; falls back to
//! `MAGICIAN_BEARER_TOKEN`),
//! `MEET_BOT_THREAD` (default `meeting-bot` — a dedicated chat thread so the bot
//! doesn't pollute the general chat), `MEET_BOT_TTS_VOICE` / `MEET_BOT_TTS_MODEL`.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::magician_v2::media_seam::responder::{MeetingResponder, ResponderError, SpokenReply};
use crate::magician_v2::media_seam::{
    OpenAiTtsProvider, StreamAudioFormat, TtsProvider, TtsRequest,
};

/// OpenAI's `pcm` TTS output is 24 kHz mono PCM16.
const OPENAI_TTS_PCM_SAMPLE_RATE: u32 = 24_000;
pub const DEFAULT_MAGICIAN_BASE_URL: &str = "http://127.0.0.1:3002/api/magician/v2";

pub struct MagicianAgentResponder {
    http: reqwest::Client,
    base_url: String,
    principal: String,
    workspace: String,
    bearer_token: Option<String>,
    thread: String,
    tts: OpenAiTtsProvider,
    voice: Option<String>,
    tts_model: Option<String>,
    session_id: Mutex<Option<String>>,
}

#[derive(Deserialize)]
struct ChatActive {
    session: SessionInfo,
}

#[derive(Deserialize)]
struct SessionInfo {
    id: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    assistant_message: Option<AssistantMessage>,
}

#[derive(Deserialize)]
struct AssistantMessage {
    #[serde(default)]
    content: Option<MessageContent>,
    #[serde(default)]
    speech_segments: Option<Vec<SpeechSegment>>,
}

#[derive(Deserialize)]
struct MessageContent {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct SpeechSegment {
    text: String,
}

impl MagicianAgentResponder {
    pub fn from_env(openai_api_key: impl Into<String>) -> Self {
        let base_url = std::env::var("MEET_BOT_MAGICIAN_URL")
            .unwrap_or_else(|_| DEFAULT_MAGICIAN_BASE_URL.to_string());
        let principal =
            std::env::var("MEET_BOT_PRINCIPAL").unwrap_or_else(|_| "anonymous".to_string());
        let workspace =
            std::env::var("MEET_BOT_WORKSPACE").unwrap_or_else(|_| "default".to_string());
        let thread = std::env::var("MEET_BOT_THREAD").unwrap_or_else(|_| "meeting-bot".to_string());
        let bearer_token = std::env::var("MEET_BOT_BEARER_TOKEN")
            .or_else(|_| std::env::var("MAGICIAN_BEARER_TOKEN"))
            .ok()
            .map(|token| token.trim().to_owned())
            .filter(|token| !token.is_empty());
        Self {
            http: reqwest::Client::new(),
            base_url,
            principal,
            workspace,
            bearer_token,
            thread,
            tts: OpenAiTtsProvider::new(openai_api_key),
            voice: std::env::var("MEET_BOT_TTS_VOICE").ok(),
            tts_model: std::env::var("MEET_BOT_TTS_MODEL").ok(),
            session_id: Mutex::new(None),
        }
    }

    /// Override the chat thread this responder posts to (per-meeting). `None` or
    /// a blank value keeps the env/default thread set in [`Self::from_env`].
    pub fn with_thread(mut self, thread: Option<String>) -> Self {
        if let Some(t) = thread
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
        {
            self.thread = t;
        }
        self
    }

    /// Bind the invoking agent's `(principal, workspace)` so replies post into
    /// that agent's chat store instead of the anonymous/default fallback.
    /// Explicit `MEET_BOT_PRINCIPAL`/`MEET_BOT_WORKSPACE` env pins still win;
    /// `None` keeps the env/default scope from [`Self::from_env`].
    pub fn with_scope(mut self, scope: Option<(String, String)>) -> Self {
        if let Some((principal, workspace)) = scope {
            let principal_pinned = std::env::var("MEET_BOT_PRINCIPAL")
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false);
            if !principal_pinned {
                self.principal = principal;
            }
            let workspace_pinned = std::env::var("MEET_BOT_WORKSPACE")
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false);
            if !workspace_pinned {
                self.workspace = workspace;
            }
        }
        self
    }

    /// Get (and cache) the chat session id for the bot's dedicated thread.
    async fn ensure_session(&self) -> Result<String, ResponderError> {
        let mut guard = self.session_id.lock().await;
        if let Some(id) = guard.as_ref() {
            return Ok(id.clone());
        }
        if self.bearer_token.is_none()
            && (self.principal != "anonymous" || self.workspace != "default")
        {
            return Err(ResponderError::Failed(
                "scoped meeting agent requires MEET_BOT_BEARER_TOKEN or MAGICIAN_BEARER_TOKEN"
                    .to_string(),
            ));
        }
        let url = format!(
            "{}/chat/active?ui_thread_id={}&history_lane=automated",
            self.base_url, self.thread
        );
        let request = self.http.get(&url);
        let request = if let Some(token) = &self.bearer_token {
            request.bearer_auth(token)
        } else {
            request
        };
        let resp = request
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| ResponderError::Failed(format!("chat/active transport: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ResponderError::Failed(format!(
                "chat/active {status}: {body}"
            )));
        }
        let parsed: ChatActive = resp
            .json()
            .await
            .map_err(|e| ResponderError::Failed(format!("chat/active decode: {e}")))?;
        *guard = Some(parsed.session.id.clone());
        Ok(parsed.session.id)
    }

    /// Send the query to the agent and return its (concise, spoken) answer text.
    async fn ask_agent(&self, utterance: &str, context: &str) -> Result<String, ResponderError> {
        let session_id = self.ensure_session().await?;
        let text = format!(
            "[Live meeting — you are participating by voice]\n{context}\n\nA participant just \
             addressed you and said: \"{utterance}\"\n\nReply to them now, briefly, to be spoken aloud."
        );
        let url = format!("{}/chat/sessions/{}/messages", self.base_url, session_id);
        let request = self.http.post(&url);
        let request = if let Some(token) = &self.bearer_token {
            request.bearer_auth(token)
        } else {
            request
        };
        let resp = request
            .timeout(Duration::from_secs(90))
            .json(&json!({ "text": text, "voice_origin": true, "source_surface": "voice" }))
            .send()
            .await
            .map_err(|e| ResponderError::Failed(format!("chat message transport: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            // The session may have gone away — drop it so the next turn recreates.
            *self.session_id.lock().await = None;
            return Err(ResponderError::Failed(format!(
                "chat message {status}: {body}"
            )));
        }
        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| ResponderError::Failed(format!("chat message decode: {e}")))?;

        let Some(message) = parsed.assistant_message else {
            return Ok(String::new());
        };
        // Prefer the clean speech segments; fall back to content text (tags stripped).
        if let Some(segments) = message.speech_segments {
            let joined = segments
                .iter()
                .map(|s| s.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !joined.is_empty() {
                return Ok(joined);
            }
        }
        Ok(strip_tags(
            &message.content.and_then(|c| c.text).unwrap_or_default(),
        ))
    }
}

/// Strip `<...>` tags (e.g. `<speech>…</speech>`) from a reply, leaving plain text.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {},
        }
    }
    out.trim().to_string()
}

#[async_trait]
impl MeetingResponder for MagicianAgentResponder {
    /// ask_agent posts the utterance + reply through the chat API, so the
    /// session's transcript lane must not surface the same exchange again.
    fn persists_to_chat(&self) -> bool {
        true
    }

    async fn respond(&self, utterance: &str, context: &str) -> Result<SpokenReply, ResponderError> {
        let text = self.ask_agent(utterance, context).await?;
        if text.is_empty() {
            return Ok(SpokenReply {
                text,
                pcm: bytes::Bytes::new(),
                format: StreamAudioFormat::default(),
            });
        }
        let request = TtsRequest {
            text: text.clone(),
            voice: self.voice.clone(),
            rate: None,
            model: self.tts_model.clone(),
            format: Some("pcm".to_string()),
            message_id: None,
            emotion: None,
            style: None,
            pace: None,
            voice_mode: None,
            emphasis: None,
        };
        let synth = self
            .tts
            .synthesize(request)
            .await
            .map_err(|e| ResponderError::Failed(format!("tts: {e}")))?;
        Ok(SpokenReply {
            text,
            pcm: synth.audio,
            format: StreamAudioFormat {
                sample_rate_hz: OPENAI_TTS_PCM_SAMPLE_RATE,
                channels: 1,
                sample_format: Default::default(),
            },
        })
    }
}
