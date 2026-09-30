//! Warm harness session — one process per run, one turn per loop iteration.
//!
//! Task 7 of `docs/plans/2026-08-23-magician-plane-vertical-slice-plan.md`.
//! This module does **not** decide how a Magician run ends; the loop still owns settle.
//! Task 8 is what wires a session into the Magician iteration. The Claude
//! engine itself now spawns a process group, writes stream-json turns, and
//! revokes the grant on every exit path.
//!
//! A grant token never appears on argv. It is written into an mcp-config file
//! the process reads, the same way Citizen keeps its token out of `ps`.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::grant::plane_grant_registry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessStopReason {
    Settled,
    TurnBudgetSpent,
    Refused,
    Cancelled,
    /// Pause the Magician run and ask a human. Not a failure, not cancellation.
    NeedsApproval,
    /// Park the Magician run on the children the harness asked for.
    Delegate,
    /// The harness stopped making progress: no stdout line, and so no tool
    /// call, for the whole idle bound. Its wall-clock ceiling had not been
    /// reached — without this the turn held the run for the full ceiling with
    /// nothing to show and nothing logged.
    Stalled,
}

/// How this spawn treats **native** (non-plane) tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NativeToolPosture {
    /// Native registry emptied (`--tools ""`). Plane is the only hands.
    Stripped,
    /// Documented built-ins removed; MCP remains. A later CLI built-in
    /// could leak until the denylist is updated.
    Denylisted,
    /// Native tools remain; OS/CLI sandbox blocks write/shell.
    Sandboxed,
    /// Native tools fully live. Plane still gates Magician tools.
    #[default]
    Live,
}

impl NativeToolPosture {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stripped => "stripped",
            Self::Denylisted => "denylisted",
            Self::Sandboxed => "sandboxed",
            Self::Live => "live",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HarnessCapabilities {
    pub supports_resume: bool,
    pub tools_list_changed: bool,
    /// The engine emits its reply through the turn's `HarnessStreamSink` as
    /// it is produced (Claude stream-json, codex_app_server's driver tap,
    /// grok's partial messages, agy's step updates); `codex exec --json`
    /// emits completed items only. The roster's promise, for surfaces that
    /// describe an engine — the chat turn gates its one-chunk send on
    /// whether anything actually streamed, so a delta-less turn on a
    /// streaming engine still shows its reply live.
    pub streams_text_deltas: bool,
    pub native_tool_posture: NativeToolPosture,
}

#[derive(Debug, Clone)]
pub struct PlaneEndpoint {
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct HarnessSessionRequest {
    /// A proposal-only turn: no Magician work tools and no native write authority.
    pub planning_only: bool,
    pub endpoint: PlaneEndpoint,
    /// `plt_…` — into the mcp-config file, never argv.
    pub grant: String,
    pub system_prompt: String,
    pub model: Option<String>,
    /// Magician chat profile resolved for Pi; other harnesses ignore it.
    pub pi_profile: Option<magicllm::config::LlmConfig>,
    /// Current chat turn's approved image inputs, encoded for Pi RPC.
    pub pi_images: Vec<serde_json::Value>,
    pub cwd: PathBuf,
    pub env_allowlist: Vec<String>,
    pub cancel: Option<CancellationToken>,
    pub resume_session_id: Option<String>,
    /// Wall-clock bound for one `turn`. Zero means no Magician-side ceiling
    /// (Claude may still settle or the run cancel token may fire).
    pub turn_timeout: Duration,
    /// How long a turn may make no progress before it is ended as stalled.
    /// `None` disables the watchdog.
    pub turn_idle_timeout: Option<Duration>,
    /// A Magician-owned directory that outlives this session: the CLI's
    /// isolated home (`CODEX_HOME` / `GROK_HOME`) and its persisted native
    /// session live here so a later turn can resume. `None` = a private
    /// temp home that is removed with the session (the pre-parity shape).
    pub native_home: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct HarnessTurnInput {
    pub text: String,
    pub operator_steer: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HarnessUsage {
    /// Every input token the model read, cached or not.
    pub input_tokens: u64,
    /// The part of `input_tokens` served from the provider's prompt cache.
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_reported: bool,
    pub cache_creation_tokens: Option<u64>,
    /// CLI-reported USD estimate; never inferred from a subscription or model name.
    pub cost_usd: Option<f64>,
    pub model: Option<String>,
}

impl HarnessUsage {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    pub fn availability(&self) -> magicllm::types::UsageAvailability {
        magicllm::types::UsageAvailability {
            tokens: true,
            cache_read: self.cache_read_reported,
            cache_write: self.cache_creation_tokens.is_some(),
            cost: self.cost_usd.is_some(),
        }
    }

    /// Sum distinct completed responses; a missing bucket makes its total unknown.
    pub fn accumulate(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
        self.cache_read_reported &= other.cache_read_reported;
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .zip(other.cache_creation_tokens)
            .map(|(a, b)| a.saturating_add(b));
        self.cost_usd = self
            .cost_usd
            .zip(other.cost_usd)
            .map(|(a, b)| a + b)
            .filter(|cost| cost.is_finite());
        if self.model != other.model {
            self.model = None;
        }
    }
}

#[derive(Debug, Clone)]
pub struct HarnessTurnSettled {
    pub assistant_text: String,
    pub stop_reason: HarnessStopReason,
    pub usage: Option<HarnessUsage>,
    pub native_session_id: Option<String>,
}

impl HarnessTurnSettled {
    /// Some CLIs deliver provider failures as a refused turn, not an RPC error.
    /// Cancellation, budget stops and ordinary refusals are not service outages.
    pub fn service_health(
        &self,
    ) -> Option<Result<(), crate::magician_v2::realtime_events::ServiceFailure>> {
        use crate::magician_v2::realtime_events::ServiceFailure;
        match self.stop_reason {
            HarnessStopReason::Settled => Some(Ok(())),
            HarnessStopReason::Refused => ServiceFailure::from_error(&self.assistant_text).map(Err),
            _ => None,
        }
    }
}

impl HarnessSessionRequest {
    /// A successful selected profile cannot recover a different profile's outage.
    /// Hash profile metadata instead of placing endpoints or credential names in HITL.
    pub fn health_service(&self, engine: &str) -> String {
        use sha2::{Digest, Sha256};
        let identity = if engine == "pi" {
            serde_json::json!({"model": self.model, "profile": self.pi_profile})
        } else {
            serde_json::json!({"model": self.model})
        };
        let digest = format!("{:x}", Sha256::digest(identity.to_string().as_bytes()));
        let choice = if self.pi_profile.is_some() {
            "profile"
        } else {
            "model"
        };
        format!("harness:{engine} [{choice} {}]", &digest[..12])
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("{0}")]
    Message(String),
}

/// Clone to fan in: a clone shares the receiver, so an engine that hands its
/// driver a callback moves a clone into it and the chat turn's pump still
/// hears every delta in order.
#[derive(Clone)]
pub struct HarnessStreamSink {
    tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl HarnessStreamSink {
    pub fn drain() -> Self {
        Self { tx: None }
    }

    pub fn channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { tx: Some(tx) }, rx)
    }

    pub fn emit(&self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(delta.to_string());
        }
    }
}

#[async_trait]
pub trait HarnessEngine: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> HarnessCapabilities;
    async fn start(
        &self,
        req: &HarnessSessionRequest,
    ) -> Result<Box<dyn HarnessSession>, HarnessError>;
}

#[async_trait]
pub trait HarnessSession: Send + Sync {
    /// Classify transport diagnostics without exposing them as assistant text.
    fn service_health(
        &self,
        outcome: &HarnessTurnSettled,
    ) -> Option<Result<(), crate::magician_v2::realtime_events::ServiceFailure>> {
        outcome.service_health()
    }

    async fn turn(
        &mut self,
        input: &HarnessTurnInput,
        sink: &HarnessStreamSink,
    ) -> Result<HarnessTurnSettled, HarnessError>;
    async fn shutdown(&mut self);
}

/// JSON body written to the mcp-config file. The grant lives here, not on argv.
pub fn mcp_config_document(endpoint: &PlaneEndpoint, grant: &str) -> Value {
    json!({
        "mcpServers": {
            "magician-plane": {
                "type": "http",
                "url": endpoint.url,
                "headers": {
                    "Authorization": format!("Bearer {grant}")
                }
            }
        }
    })
}

/// Revoke a live grant. Called from every session exit path.
pub async fn revoke_session_grant(grant: &str) {
    if grant.starts_with("plt_") {
        plane_grant_registry().revoke(grant).await;
    }
}

#[cfg(test)]
impl HarnessSessionRequest {
    pub fn test() -> Self {
        Self {
            endpoint: PlaneEndpoint {
                url: "http://127.0.0.1:8899/api/magician/v2/plane/mcp".to_string(),
            },
            grant: "plt_test_grant_token".to_string(),
            system_prompt: "You are Magician's plane.".to_string(),
            model: Some("claude-opus-4-6".to_string()),
            pi_profile: None,
            pi_images: Vec::new(),
            cwd: PathBuf::from("/tmp"),
            env_allowlist: Vec::new(),
            cancel: None,
            resume_session_id: None,
            turn_timeout: Duration::from_secs(crate::config::DEFAULT_AGENTIC_MAX_DURATION_SECS),
            turn_idle_timeout: Some(Duration::from_secs(
                crate::config::default_harness_turn_idle_seconds(),
            )),
            native_home: None,
            planning_only: false,
        }
    }
}

#[cfg(test)]
mod stream_sink {
    use super::HarnessStreamSink;

    #[tokio::test]
    async fn drain_is_silent_and_channel_forwards() {
        let drain = HarnessStreamSink::drain();
        drain.emit("ignored");
        let (sink, mut rx) = HarnessStreamSink::channel();
        sink.emit("hello");
        drop(sink);
        assert_eq!(rx.recv().await.as_deref(), Some("hello"));
        assert!(rx.recv().await.is_none());
    }
}

#[cfg(test)]
mod outcome_contract {
    use super::*;

    #[test]
    fn service_health_refused_credentials_are_reported_but_cancel_and_budget_are_not() {
        let mut turn = HarnessTurnSettled {
            assistant_text: "Pi turn failed: No API key found for the selected model".into(),
            stop_reason: HarnessStopReason::Refused,
            usage: None,
            native_session_id: None,
        };
        assert_eq!(
            turn.service_health(),
            Some(Err(
                crate::magician_v2::realtime_events::ServiceFailure::Authentication
            ))
        );
        for stop in [
            HarnessStopReason::Cancelled,
            HarnessStopReason::TurnBudgetSpent,
            HarnessStopReason::Stalled,
        ] {
            turn.stop_reason = stop;
            assert_eq!(turn.service_health(), None);
        }
        turn.stop_reason = HarnessStopReason::Refused;
        turn.assistant_text = "Action denied by task policy".into();
        assert_eq!(turn.service_health(), None);
        turn.stop_reason = HarnessStopReason::Settled;
        assert_eq!(turn.service_health(), Some(Ok(())));
    }

    #[test]
    fn the_engine_never_constructs_an_agentic_outcome() {
        let source = include_str!("engine.rs");
        let impl_src = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            !impl_src.contains("AgenticOutcome"),
            "the harness must not decide how the run ends"
        );
        let claude = include_str!("engines/claude_code.rs");
        let claude_impl = claude.split("#[cfg(test)]").next().unwrap_or(claude);
        assert!(
            !claude_impl.contains("AgenticOutcome"),
            "the Claude engine must not decide how the run ends"
        );
    }
}
