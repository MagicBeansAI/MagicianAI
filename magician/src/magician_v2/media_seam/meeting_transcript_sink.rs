//! Live meeting-transcript sink: streams every heard turn into the bot's
//! per-meeting chat thread, over the same loopback chat API the responder uses.
//!
//! Display-only BY CONSTRUCTION. It posts to `POST /chat/sessions/{id}/transcript`,
//! which persists + broadcasts the line WITHOUT dispatching the agent (the chat
//! service's emit-only path). So streaming the transcript can never trigger a
//! reply — only the wake-word gate decides when the bot actually responds. The
//! two lanes (display vs respond) are therefore physically separate.
//!
//! Non-blocking BY CONSTRUCTION, too. `post_turn` only enqueues onto a bounded
//! channel (dropping with a warn when full); a spawned worker owns the HTTP
//! posting — in order, with exponential backoff after failures. The STT event
//! loop that calls `record_turn` therefore never waits on the chat API: a
//! transcript hiccup costs dropped display lines, never wake/barge-in latency.
//! The worker drains and exits when the owning session drops the sink.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::warn;

use crate::magician_v2::media_seam::meeting_magician_agent_responder::DEFAULT_MAGICIAN_BASE_URL;
use crate::magician_v2::media_seam::meeting_session_engine::TranscriptTurn;

/// Streams finalized transcript turns to a chat thread (or drops them).
#[async_trait]
pub trait TranscriptSink: Send + Sync {
    /// Hand one finalized turn to the sink. MUST be effectively instant:
    /// implementations queue or drop — never run network I/O inline, because
    /// the caller is the real-time STT loop that gates wake responses.
    async fn post_turn(&self, turn: &TranscriptTurn);
}

/// Default sink: drop every turn (tests, or transcript streaming disabled).
pub struct NoopTranscriptSink;

#[async_trait]
impl TranscriptSink for NoopTranscriptSink {
    async fn post_turn(&self, _turn: &TranscriptTurn) {}
}

#[derive(Deserialize)]
struct ChatActive {
    session: SessionInfo,
}

#[derive(Deserialize)]
struct SessionInfo {
    id: String,
}

/// How many turns may queue while the chat API is slow before new ones drop.
const QUEUE_CAPACITY: usize = 256;
/// Cap on the worker's between-failure backoff.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Posts each turn to the per-meeting chat thread via the loopback chat API.
/// `post_turn` enqueues; the spawned [`SinkWorker`] resolves (and caches) the
/// thread's chat session id the same way the responder does (`GET /chat/active`)
/// and `POST`s each line to the non-dispatching transcript endpoint.
pub struct ChatThreadTranscriptSink {
    tx: mpsc::Sender<TranscriptTurn>,
}

impl ChatThreadTranscriptSink {
    /// Build from the meeting-bot env (mirrors the responder's wiring) + the
    /// per-meeting `thread` id + the invoking agent's `scope`. Spawns the
    /// posting worker (so this must run inside a tokio runtime).
    ///
    /// `session_title` names the thread's chat session (the generic
    /// get-or-create path leaves it "Untitled session"); it is (re)applied on
    /// every session resolve, so a mid-meeting rotation gets titled too.
    /// `announce` is posted once at startup — the warm-up makes the per-meeting
    /// thread VISIBLE the moment the bot joins, instead of at the first heard
    /// turn (which can be a minute of silence away).
    ///
    /// Precedence mirrors the responders' `with_scope`: explicit
    /// `MEET_BOT_PRINCIPAL`/`MEET_BOT_WORKSPACE` env pins win, then the agent's
    /// scope, then anonymous/default — keeping the transcript lane and the
    /// responder lanes resolving the SAME chat session for the same meeting.
    pub fn from_env(
        thread: Option<String>,
        scope: Option<(String, String)>,
        session_title: Option<String>,
        announce: Option<String>,
    ) -> Self {
        let base_url = std::env::var("MEET_BOT_MAGICIAN_URL")
            .unwrap_or_else(|_| DEFAULT_MAGICIAN_BASE_URL.to_string());
        let (scope_principal, scope_workspace) = scope.unzip();
        let principal = std::env::var("MEET_BOT_PRINCIPAL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or(scope_principal)
            .unwrap_or_else(|| "anonymous".to_string());
        let workspace = std::env::var("MEET_BOT_WORKSPACE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or(scope_workspace)
            .unwrap_or_else(|| "default".to_string());
        let bearer_token = std::env::var("MEET_BOT_BEARER_TOKEN")
            .or_else(|_| std::env::var("MAGICIAN_BEARER_TOKEN"))
            .ok()
            .map(|token| token.trim().to_owned())
            .filter(|token| !token.is_empty());
        let thread = thread
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .or_else(|| {
                std::env::var("MEET_BOT_THREAD")
                    .ok()
                    .filter(|t| !t.trim().is_empty())
            })
            .unwrap_or_else(|| "meeting-bot".to_string());
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        let worker = SinkWorker {
            http: reqwest::Client::new(),
            base_url,
            principal,
            workspace,
            bearer_token,
            thread,
            session_id: None,
            consecutive_failures: 0,
            session_title,
            announce,
        };
        tokio::spawn(worker.run(rx));
        Self { tx }
    }
}

#[async_trait]
impl TranscriptSink for ChatThreadTranscriptSink {
    async fn post_turn(&self, turn: &TranscriptTurn) {
        if turn.text.trim().is_empty() {
            return;
        }
        // Non-blocking by contract: enqueue or drop, never wait.
        if let Err(e) = self.tx.try_send(turn.clone()) {
            warn!(target: "meet_bot", error = %e, "meeting transcript: queue full/closed; dropping line");
        }
    }
}

/// Owns the HTTP side of the sink on its own task: ordered posting, cached
/// session id, exponential backoff after failures (so a dead chat API isn't
/// hammered twice per utterance for the rest of the meeting). No locks — the
/// worker is the sole owner of its state.
struct SinkWorker {
    http: reqwest::Client,
    base_url: String,
    principal: String,
    workspace: String,
    /// Opaque credential whose server-side claims own `principal/workspace`.
    bearer_token: Option<String>,
    thread: String,
    session_id: Option<String>,
    consecutive_failures: u32,
    /// Title (re)applied to the thread's chat session on every resolve.
    session_title: Option<String>,
    /// One-shot join announcement posted at warm-up.
    announce: Option<String>,
}

impl SinkWorker {
    async fn run(mut self, mut rx: mpsc::Receiver<TranscriptTurn>) {
        // Warm-up: resolve (get-or-create) the thread's chat session NOW so the
        // per-meeting thread is visible the moment the bot joins — not at the
        // first heard turn, which can be a minute of silence away. ensure_session
        // also titles the session. Best-effort: on failure the thread simply
        // appears at the first turn instead.
        match self.ensure_session().await {
            Ok(_) => {
                if let Some(text) = self.announce.take() {
                    let turn = TranscriptTurn {
                        at_ms: 0,
                        speaker: None,
                        text,
                    };
                    if let Err(e) = self.try_post(&turn).await {
                        warn!(target: "meet_bot", error = %e, "meeting transcript: join announce failed");
                    }
                }
            },
            Err(e) => {
                warn!(
                    target: "meet_bot",
                    error = %e,
                    "meeting transcript: warm-up session resolve failed; the thread appears at the first turn"
                );
            },
        }
        while let Some(turn) = rx.recv().await {
            // One immediate same-line retry: the common failure is a 4xx from a
            // just-archived/rotated session, which try_post answers by clearing
            // the cached session id — the retry re-resolves the thread's CURRENT
            // active session and recovers the rotation-straddling line instead
            // of dropping it.
            let result = match self.try_post(&turn).await {
                Ok(()) => Ok(()),
                Err(_first) => self.try_post(&turn).await,
            };
            match result {
                Ok(()) => self.consecutive_failures = 0,
                Err(e) => {
                    self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                    warn!(
                        target: "meet_bot",
                        error = %e,
                        consecutive_failures = self.consecutive_failures,
                        "meeting transcript: post failed (line dropped)"
                    );
                    let backoff = Duration::from_secs(1u64 << self.consecutive_failures.min(6))
                        .min(MAX_BACKOFF);
                    tokio::time::sleep(backoff).await;
                },
            }
        }
    }

    /// Get (and cache) the chat session id for the bot's thread. Converges on the
    /// SAME session the responder resolves for this thread (get-or-create).
    async fn ensure_session(&mut self) -> Result<String, String> {
        if let Some(id) = &self.session_id {
            return Ok(id.clone());
        }
        if self.bearer_token.is_none()
            && (self.principal != "anonymous" || self.workspace != "default")
        {
            return Err(
                "scoped meeting transcript requires MEET_BOT_BEARER_TOKEN or MAGICIAN_BEARER_TOKEN"
                    .to_string(),
            );
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
            .map_err(|e| format!("chat/active transport: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("chat/active {status}: {body}"));
        }
        let parsed: ChatActive = resp
            .json()
            .await
            .map_err(|e| format!("chat/active decode: {e}"))?;
        self.session_id = Some(parsed.session.id.clone());
        // Title the freshly-resolved session (the get-or-create path leaves it
        // "Untitled session"). Re-applied on every resolve so a mid-meeting
        // rotation ('+ New') gets the meeting title too. Best-effort.
        if let Some(title) = self.session_title.clone() {
            if let Err(e) = self.set_session_title(&parsed.session.id, &title).await {
                warn!(target: "meet_bot", error = %e, "meeting transcript: failed to title the session");
            }
        }
        Ok(parsed.session.id)
    }

    /// `PATCH /chat/sessions/{id}` with the meeting's display title.
    async fn set_session_title(&self, session_id: &str, title: &str) -> Result<(), String> {
        let url = format!("{}/chat/sessions/{}", self.base_url, session_id);
        let request = self.http.patch(&url);
        let request = if let Some(token) = &self.bearer_token {
            request.bearer_auth(token)
        } else {
            request
        };
        let resp = request
            .timeout(Duration::from_secs(15))
            .json(&json!({ "title": title }))
            .send()
            .await
            .map_err(|e| format!("session title transport: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("session title {status}: {body}"));
        }
        Ok(())
    }

    async fn try_post(&mut self, turn: &TranscriptTurn) -> Result<(), String> {
        let session_id = self.ensure_session().await?;
        let url = format!("{}/chat/sessions/{}/transcript", self.base_url, session_id);
        let request = self.http.post(&url);
        let request = if let Some(token) = &self.bearer_token {
            request.bearer_auth(token)
        } else {
            request
        };
        let resp = request
            .timeout(Duration::from_secs(15))
            .json(&json!({ "text": turn.text, "speaker": turn.speaker }))
            .send()
            .await
            .map_err(|e| format!("transcript transport: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            // Drop the cached session id so the next turn re-resolves — the
            // session may have been archived/rotated (the endpoint rejects
            // archived sessions precisely so this branch fires).
            self.session_id = None;
            return Err(format!("transcript {status}: {body}"));
        }
        Ok(())
    }
}
