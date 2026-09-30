//! Coding-run control registry — the interactive control plane.
//!
//! A process-global map of **in-flight** coding runs to their live
//! [`CodingControlHandle`], so an external request (the VibeDev cockpit's Stop
//! / steer) can reach a turn that is mid-flight. `run_turn` registers the
//! handle under every identifier the UI might hold (task_id / execution_id /
//! shadow_workspace_id) for exactly the turn's lifetime and unregisters it the
//! moment the turn settles, so a stale handle can never be steered.
//!
//! This is the minimal, focused slice of the warm-`PiSessionManager` (plan
//! §13.3 #2): it does not keep a process warm across invocations, it just
//! exposes the already-live turn's command channel while it runs. Stage 4
//! adds [`CodingControlHandle::Codex`]. Phase 4 adds [`CodingControlHandle::Grok`].

use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use once_cell::sync::Lazy;
use tokio::sync::Mutex;

use super::{
    codex_lifecycle::{MAX_CODEX_FOLLOW_UPS, MAX_GROK_FOLLOW_UPS},
    pi::PiSessionHandle,
};

/// Exhaustive live-run handle. Codex/Grok steer/stop/follow-up go through
/// generation-fenced turn handles; a stale generation cannot command a
/// replacement.
#[derive(Clone)]
pub enum CodingControlHandle {
    Pi(PiSessionHandle),
    Codex(CodexTurnHandle),
    Grok(GrokTurnHandle),
    Claude(ClaudeTurnHandle),
    Agy(AgyTurnHandle),
}

impl CodingControlHandle {
    async fn steer(&self, message: &str) -> Result<()> {
        match self {
            Self::Pi(handle) => handle.steer(message, None).await,
            Self::Codex(handle) => handle.steer(message).await,
            Self::Grok(handle) => handle.steer(message).await,
            Self::Claude(handle) => handle.steer(message).await,
            Self::Agy(handle) => handle.steer(message).await,
        }
    }

    async fn follow_up(&self, message: &str) -> Result<()> {
        match self {
            Self::Pi(handle) => handle.follow_up(message, None).await,
            Self::Codex(handle) => handle.follow_up(message).await,
            Self::Grok(handle) => handle.follow_up(message).await,
            Self::Claude(handle) => handle.follow_up(message).await,
            Self::Agy(handle) => handle.follow_up(message).await,
        }
    }

    async fn stop(&self) -> Result<()> {
        match self {
            Self::Pi(handle) => handle.abort().await,
            Self::Codex(handle) => handle.stop().await,
            Self::Grok(handle) => handle.stop().await,
            Self::Claude(handle) => handle.stop().await,
            Self::Agy(handle) => handle.stop().await,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexControlCommand {
    Steer {
        expected_turn_id: String,
        input: String,
    },
    Interrupt {
        turn_id: String,
    },
}

/// Live Codex turn. Follow-ups are a bounded FIFO on this handle because
/// Codex has no Pi-equivalent internal queue.
#[derive(Clone)]
pub struct CodexTurnHandle {
    pub generation: u64,
    pub native_session_id: String,
    pub execution_id: String,
    live_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    expected_turn_id: std::sync::Arc<tokio::sync::Mutex<Option<String>>>,
    follow_ups: std::sync::Arc<tokio::sync::Mutex<std::collections::VecDeque<String>>>,
    tx: tokio::sync::mpsc::UnboundedSender<CodexControlCommand>,
}

impl CodexTurnHandle {
    pub fn bind(
        generation: u64,
        native_session_id: impl Into<String>,
        execution_id: impl Into<String>,
    ) -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<CodexControlCommand>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = Self {
            generation,
            native_session_id: native_session_id.into(),
            execution_id: execution_id.into(),
            live_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(generation)),
            expected_turn_id: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            follow_ups: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::VecDeque::new(),
            )),
            tx,
        };
        (handle, rx)
    }

    pub fn retire(&self) {
        self.live_generation.store(
            self.generation.saturating_add(1),
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    pub async fn set_expected_turn(&self, turn_id: Option<String>) {
        *self.expected_turn_id.lock().await = turn_id;
    }

    pub async fn take_follow_up(&self) -> Option<String> {
        self.follow_ups.lock().await.pop_front()
    }

    fn generation_live(&self) -> Result<()> {
        let live = self
            .live_generation
            .load(std::sync::atomic::Ordering::SeqCst);
        if live != self.generation {
            return Err(anyhow::anyhow!(
                "stale Codex control generation {0} (live {live})",
                self.generation
            ));
        }
        Ok(())
    }

    async fn steer(&self, message: &str) -> Result<()> {
        self.generation_live()?;
        let expected = self
            .expected_turn_id
            .lock()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Codex steer requires an active turn id"))?;
        self.tx.send(CodexControlCommand::Steer {
            expected_turn_id: expected,
            input: message.to_string(),
        })?;
        Ok(())
    }

    async fn follow_up(&self, message: &str) -> Result<()> {
        self.generation_live()?;
        let mut queue = self.follow_ups.lock().await;
        if queue.len() >= MAX_CODEX_FOLLOW_UPS {
            return Err(anyhow::anyhow!(
                "Codex follow-up queue is at its cap of {MAX_CODEX_FOLLOW_UPS}"
            ));
        }
        queue.push_back(message.to_string());
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.generation_live()?;
        let turn_id = self
            .expected_turn_id
            .lock()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Codex stop requires an active turn id"))?;
        self.tx.send(CodexControlCommand::Interrupt { turn_id })?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokControlCommand {
    Interrupt { session_id: Option<String> },
}

/// Live Grok ACP turn. Follow-ups and V1 mid-turn steer share a bounded FIFO
/// because Grok has no Pi-equivalent internal queue and ACP does not expose
/// a stable mid-stream steer.
#[derive(Clone)]
pub struct GrokTurnHandle {
    pub generation: u64,
    pub native_session_id: String,
    pub execution_id: String,
    live_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    expected_turn_id: std::sync::Arc<tokio::sync::Mutex<Option<String>>>,
    follow_ups: std::sync::Arc<tokio::sync::Mutex<std::collections::VecDeque<String>>>,
    tx: tokio::sync::mpsc::UnboundedSender<GrokControlCommand>,
}

impl GrokTurnHandle {
    pub fn bind(
        generation: u64,
        native_session_id: impl Into<String>,
        execution_id: impl Into<String>,
    ) -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<GrokControlCommand>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = Self {
            generation,
            native_session_id: native_session_id.into(),
            execution_id: execution_id.into(),
            live_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(generation)),
            expected_turn_id: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            follow_ups: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::VecDeque::new(),
            )),
            tx,
        };
        (handle, rx)
    }

    pub fn retire(&self) {
        self.live_generation.store(
            self.generation.saturating_add(1),
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    pub async fn set_expected_turn(&self, turn_id: Option<String>) {
        *self.expected_turn_id.lock().await = turn_id;
    }

    pub async fn take_follow_up(&self) -> Option<String> {
        self.follow_ups.lock().await.pop_front()
    }

    fn generation_live(&self) -> Result<()> {
        let live = self
            .live_generation
            .load(std::sync::atomic::Ordering::SeqCst);
        if live != self.generation {
            return Err(anyhow::anyhow!(
                "stale Grok control generation {0} (live {live})",
                self.generation
            ));
        }
        Ok(())
    }

    async fn steer(&self, message: &str) -> Result<()> {
        // V1: queue like follow-up. Do not depend on Grok TUI follow_up_behavior.
        self.follow_up(message).await
    }

    async fn follow_up(&self, message: &str) -> Result<()> {
        self.generation_live()?;
        let mut queue = self.follow_ups.lock().await;
        if queue.len() >= MAX_GROK_FOLLOW_UPS {
            return Err(anyhow::anyhow!(
                "Grok follow-up queue is at its cap of {MAX_GROK_FOLLOW_UPS}"
            ));
        }
        queue.push_back(message.to_string());
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.generation_live()?;
        let session_id = self.expected_turn_id.lock().await.clone();
        self.tx.send(GrokControlCommand::Interrupt { session_id })?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeControlCommand {
    Interrupt,
}

/// Live Claude `-p` turn. Stop cancels the drain; V1 steer/follow-up share a
/// bounded FIFO because print mode has no mid-stream RPC.
#[derive(Clone)]
pub struct ClaudeTurnHandle {
    pub generation: u64,
    pub native_session_id: String,
    pub execution_id: String,
    live_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[allow(dead_code)]
    follow_ups: std::sync::Arc<tokio::sync::Mutex<std::collections::VecDeque<String>>>,
    tx: tokio::sync::mpsc::UnboundedSender<ClaudeControlCommand>,
}

impl ClaudeTurnHandle {
    pub fn bind(
        generation: u64,
        native_session_id: impl Into<String>,
        execution_id: impl Into<String>,
    ) -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<ClaudeControlCommand>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = Self {
            generation,
            native_session_id: native_session_id.into(),
            execution_id: execution_id.into(),
            live_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(generation)),
            follow_ups: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::VecDeque::new(),
            )),
            tx,
        };
        (handle, rx)
    }

    pub fn retire(&self) {
        self.live_generation.store(
            self.generation.saturating_add(1),
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    pub async fn take_follow_up(&self) -> Option<String> {
        self.follow_ups.lock().await.pop_front()
    }

    fn generation_live(&self) -> Result<()> {
        let live = self
            .live_generation
            .load(std::sync::atomic::Ordering::SeqCst);
        if live != self.generation {
            return Err(anyhow::anyhow!(
                "stale Claude control generation {0} (live {live})",
                self.generation
            ));
        }
        Ok(())
    }

    async fn steer(&self, _message: &str) -> Result<()> {
        self.generation_live()?;
        Err(anyhow::anyhow!(
            "Claude print mode has no mid-turn steer; send a VibeDev follow-up so Magician can --resume"
        ))
    }

    async fn follow_up(&self, _message: &str) -> Result<()> {
        self.generation_live()?;
        Err(anyhow::anyhow!(
            "Claude print mode has no mid-turn follow-up RPC; send a VibeDev follow-up so Magician can --resume"
        ))
    }

    async fn stop(&self) -> Result<()> {
        self.generation_live()?;
        self.tx.send(ClaudeControlCommand::Interrupt)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgyControlCommand {
    Interrupt,
}

/// Live Agy `-p` turn. Stop cancels the drain; V1 steer/follow-up share a
/// bounded FIFO because print mode has no mid-stream RPC. Resume is a new
/// `-p --conversation` process, not a stdin JSONL follow-up.
#[derive(Clone)]
pub struct AgyTurnHandle {
    pub generation: u64,
    pub native_session_id: String,
    pub execution_id: String,
    live_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[allow(dead_code)]
    follow_ups: std::sync::Arc<tokio::sync::Mutex<std::collections::VecDeque<String>>>,
    tx: tokio::sync::mpsc::UnboundedSender<AgyControlCommand>,
}

impl AgyTurnHandle {
    pub fn bind(
        generation: u64,
        native_session_id: impl Into<String>,
        execution_id: impl Into<String>,
    ) -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<AgyControlCommand>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = Self {
            generation,
            native_session_id: native_session_id.into(),
            execution_id: execution_id.into(),
            live_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(generation)),
            follow_ups: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::VecDeque::new(),
            )),
            tx,
        };
        (handle, rx)
    }

    pub fn retire(&self) {
        self.live_generation.store(
            self.generation.saturating_add(1),
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    pub async fn take_follow_up(&self) -> Option<String> {
        self.follow_ups.lock().await.pop_front()
    }

    fn generation_live(&self) -> Result<()> {
        let live = self
            .live_generation
            .load(std::sync::atomic::Ordering::SeqCst);
        if live != self.generation {
            return Err(anyhow::anyhow!(
                "stale Agy control generation {0} (live {live})",
                self.generation
            ));
        }
        Ok(())
    }

    async fn steer(&self, _message: &str) -> Result<()> {
        self.generation_live()?;
        Err(anyhow::anyhow!(
            "Agy print mode has no mid-turn steer; send a VibeDev follow-up so Magician can --conversation"
        ))
    }

    async fn follow_up(&self, _message: &str) -> Result<()> {
        self.generation_live()?;
        Err(anyhow::anyhow!(
            "Agy print mode has no mid-turn follow-up RPC; send a VibeDev follow-up so Magician can --conversation"
        ))
    }

    async fn stop(&self) -> Result<()> {
        self.generation_live()?;
        self.tx.send(AgyControlCommand::Interrupt)?;
        Ok(())
    }
}

/// What the control plane can do to a live run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingControlAction {
    /// Redirect the in-flight turn (delivered before the next model call).
    Steer,
    /// Queue a message delivered once the agent fully stops.
    FollowUp,
    /// Cancel the current operation (graceful Pi `abort`).
    Stop,
}

#[derive(Default)]
pub struct CodingControlRegistry {
    handles: Mutex<HashMap<String, CodingControlHandle>>,
}

static REGISTRY: Lazy<Arc<CodingControlRegistry>> =
    Lazy::new(|| Arc::new(CodingControlRegistry::default()));

/// The process-global control registry.
pub fn coding_control_registry() -> Arc<CodingControlRegistry> {
    REGISTRY.clone()
}

/// Scope-qualify a run identifier. The registry is process-global, so keys are
/// namespaced by `(principal, workspace)` — a control request can only ever
/// reach a run that belongs to the same authenticated scope, never another
/// tenant's run that happens to share a task/execution id.
pub fn scoped_control_key(principal: &str, workspace: &str, id: &str) -> String {
    format!("{principal}::{workspace}::{id}")
}

impl CodingControlRegistry {
    /// Register a live turn's handle under each non-empty key. Called by
    /// `run_turn` immediately before it starts streaming.
    pub async fn register(&self, keys: &[String], handle: CodingControlHandle) {
        if keys.iter().all(|key| key.is_empty()) {
            return;
        }
        let mut map = self.handles.lock().await;
        for key in keys {
            if !key.is_empty() {
                map.insert(key.clone(), handle.clone());
            }
        }
    }

    /// Drop every key for a settled turn. Called by `run_turn` the moment the
    /// turn resolves (success, error, or timeout) so no stale handle survives.
    pub async fn unregister(&self, keys: &[String]) {
        if keys.is_empty() {
            return;
        }
        let mut map = self.handles.lock().await;
        for key in keys {
            map.remove(key);
        }
    }

    /// Whether a run is currently steerable under this key.
    pub async fn is_active(&self, key: &str) -> bool {
        self.handles.lock().await.contains_key(key)
    }

    async fn handle(&self, key: &str) -> Option<CodingControlHandle> {
        self.handles.lock().await.get(key).cloned()
    }

    /// Apply a control action to the live run for `key`. Returns `Ok(true)` if
    /// a live run was found and the command was delivered, `Ok(false)` if
    /// there is no active run for that key (the caller should report "run
    /// not live"), or `Err` if the engine rejected the command.
    pub async fn control(
        &self,
        key: &str,
        action: CodingControlAction,
        message: Option<&str>,
    ) -> Result<bool> {
        let Some(handle) = self.handle(key).await else {
            return Ok(false);
        };
        match action {
            CodingControlAction::Steer => {
                handle.steer(message.unwrap_or_default()).await?;
            },
            CodingControlAction::FollowUp => {
                handle.follow_up(message.unwrap_or_default()).await?;
            },
            CodingControlAction::Stop => {
                handle.stop().await?;
            },
        }
        Ok(true)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn control_returns_false_for_unknown_run() {
        let registry = CodingControlRegistry::default();
        let delivered = registry
            .control("no-such-run", CodingControlAction::Stop, None)
            .await
            .expect("control on an unknown run is not an error");
        assert!(!delivered, "no live run → not delivered");
        assert!(!registry.is_active("no-such-run").await);
    }

    #[tokio::test]
    async fn unregister_is_safe_on_empty_and_unknown_keys() {
        let registry = CodingControlRegistry::default();
        registry.unregister(&[]).await; // no-op
        registry.unregister(&["never-registered".to_string()]).await; // no-op
        assert!(!registry.is_active("never-registered").await);
    }

    #[test]
    fn adapters_register_the_enum_handle() {
        let pi = include_str!("pi.rs");
        assert!(
            pi.contains("CodingControlHandle::Pi("),
            "Pi must wrap its session handle in the control enum"
        );
        let codex = include_str!("codex.rs");
        assert!(
            codex.contains("CodingControlHandle::Codex("),
            "Codex must wrap its turn handle in the control enum"
        );
        let grok = include_str!("grok.rs");
        assert!(
            grok.contains("CodingControlHandle::Grok("),
            "Grok must wrap its turn handle in the control enum"
        );
        let claude = include_str!("claude.rs");
        assert!(
            claude.contains("CodingControlHandle::Claude("),
            "Claude must wrap its turn handle in the control enum"
        );
        let agy = include_str!("agy.rs");
        assert!(
            agy.contains("CodingControlHandle::Agy("),
            "Agy must wrap its turn handle in the control enum"
        );
    }

    #[test]
    fn pi_registers_the_enum_handle_not_the_raw_session() {
        let source = include_str!("pi.rs");
        assert!(
            source.contains("CodingControlHandle::Pi("),
            "Pi must wrap its session handle in the control enum"
        );
        assert!(
            !source.contains(".register(&control_keys, control.clone())"),
            "raw PiSessionHandle must not be registered"
        );
    }

    #[tokio::test]
    async fn stale_generation_cannot_steer_or_stop_a_replacement() {
        let (handle, mut rx) = CodexTurnHandle::bind(3, "thread-1", "exec-1");
        handle.set_expected_turn(Some("turn-1".into())).await;
        handle.retire();
        assert!(handle.steer("no").await.is_err());
        assert!(handle.stop().await.is_err());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn follow_up_is_bounded_and_steer_uses_expected_turn() {
        let (handle, mut rx) = CodexTurnHandle::bind(1, "thread-1", "exec-1");
        handle.set_expected_turn(Some("turn-9".into())).await;
        handle.steer("go left").await.expect("steer");
        match rx.try_recv().expect("cmd") {
            CodexControlCommand::Steer {
                expected_turn_id,
                input,
            } => {
                assert_eq!(expected_turn_id, "turn-9");
                assert_eq!(input, "go left");
            },
            other => panic!("{other:?}"),
        }
        for index in 0..super::super::codex_lifecycle::MAX_CODEX_FOLLOW_UPS {
            handle
                .follow_up(&format!("next-{index}"))
                .await
                .expect("queue");
        }
        assert!(handle.follow_up("overflow").await.is_err());
        assert_eq!(handle.take_follow_up().await.as_deref(), Some("next-0"));
    }

    #[tokio::test]
    async fn grok_stale_generation_cannot_steer_or_stop_a_replacement() {
        let (handle, mut rx) = GrokTurnHandle::bind(3, "sess-1", "exec-1");
        handle.set_expected_turn(Some("sess-1".into())).await;
        handle.retire();
        assert!(handle.steer("no").await.is_err());
        assert!(handle.follow_up("no").await.is_err());
        assert!(handle.stop().await.is_err());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn grok_steer_and_follow_up_share_a_bounded_fifo() {
        let (handle, mut rx) = GrokTurnHandle::bind(1, "sess-1", "exec-1");
        handle.steer("go left").await.expect("steer queues");
        assert!(
            rx.try_recv().is_err(),
            "V1 Grok steer must not send a mid-stream command"
        );
        handle.stop().await.expect("stop");
        assert!(matches!(
            rx.try_recv().expect("interrupt"),
            GrokControlCommand::Interrupt { .. }
        ));
        for index in 1..super::super::codex_lifecycle::MAX_GROK_FOLLOW_UPS {
            handle
                .follow_up(&format!("next-{index}"))
                .await
                .expect("queue");
        }
        assert!(handle.follow_up("overflow").await.is_err());
        assert_eq!(handle.take_follow_up().await.as_deref(), Some("go left"));
        assert_eq!(handle.take_follow_up().await.as_deref(), Some("next-1"));
    }
}
