//! Chat-turn harness mouth (plan 2026-08-31).
//!
//! Displaces the Magician LLM call inside `process_chat_inline_turn`. Hands
//! stay on the plane. Default engine is `magician` (today's path). Any
//! launchable roster name is a chat mouth; unknown names fail closed to
//! Magician. Native-tool strip is per engine.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use magicllm::types::LLMToolSpec;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::{default_harness_turn_max_seconds, default_harness_turn_max_tool_calls};
use crate::magician_v2::agents::{
    AgentInvocationContext, ApprovalRule, FeatureMode, InvocationSurface, TrustPolicyEnforcer,
};
use crate::magician_v2::auth::sessions::NEVER_ON_THE_PLANE;
use crate::magician_v2::chat::models::{ChatLlmTranscriptEntry, StoredToolCall};
use crate::magician_v2::chat::tools_runtime::PlanePosture;
use crate::magician_v2::execution::agentic::AgenticContext;
use crate::magician_v2::execution::agentic::EffectiveToolPolicySnapshot;
use crate::magician_v2::execution::flat_loop::ToolIndex;
use crate::magician_v2::execution::plane::catalog::builtin_hot_names;
use crate::magician_v2::execution::plane::engine::{
    HarnessSessionRequest, HarnessStopReason, HarnessStreamSink, HarnessTurnInput,
    HarnessTurnSettled, HarnessUsage, PlaneEndpoint,
};
use crate::magician_v2::execution::plane::engine_pin::RunEnginePin;
use crate::magician_v2::execution::plane::engines::oneshot::chmod_private_dir;
use crate::magician_v2::execution::plane::grant::{
    drain_turn_ledger, head_within, plane_grant_registry, PlaneCatalogProfile, PlaneGrant,
    PlaneTurnToolCall, RevokeGrantOnDrop, PLANE_TURN_RESULT_CUT_MARK,
};
use crate::magician_v2::execution::plane::mouth_bridge::ChatMouthBridge;
use crate::magician_v2::execution::plane::terminal_session::scoped_runless_executors;
use crate::magician_v2::execution::plane::turn_engine::{resolve_turn_engine, TurnEngine};
use crate::magician_v2::secrets::{sanitize_json_for_provider, sanitize_text_for_provider};

#[derive(Debug, Clone)]
pub struct ChatHarnessSnapshot {
    pub engine: String,
    /// The model the chat harness runs; `default` = the CLI's own choice.
    pub harness_model: String,
    pub turn_max_tool_calls: u32,
    pub turn_max_seconds: u64,
    pub plane_endpoint: String,
}

impl Default for ChatHarnessSnapshot {
    fn default() -> Self {
        Self {
            engine: "magician".to_string(),
            harness_model: "default".to_string(),
            turn_max_tool_calls: default_harness_turn_max_tool_calls(),
            turn_max_seconds: default_harness_turn_max_seconds(),
            plane_endpoint: "http://127.0.0.1:8080/api/magician/v2/plane/mcp".to_string(),
        }
    }
}

static CHAT_SNAPSHOT: Lazy<RwLock<ChatHarnessSnapshot>> =
    Lazy::new(|| RwLock::new(ChatHarnessSnapshot::default()));

/// Roster names are valid chat mouths. Unknown names coerce to Magician.
pub fn coerce_chat_mouth_engine(name: &str) -> &'static str {
    match name.trim() {
        "pi" => "pi",
        "claude_code" => "claude_code",
        "codex" => "codex",
        "codex_app_server" => "codex_app_server",
        "grok" => "grok",
        "agy" => "agy",
        _ => "magician",
    }
}

/// The chat engine and model most recently installed. Every path that
/// changes them (`PUT /plane/chat-engine`, each config reload, onboarding)
/// installs a snapshot, so this one channel is how the process announces
/// the change; the binary relays it to open clients as
/// `chat.engine.updated`.
static CHAT_ENGINE_CHANGES: Lazy<tokio::sync::watch::Sender<(String, String)>> = Lazy::new(|| {
    let initial = ChatHarnessSnapshot::default();
    tokio::sync::watch::channel((initial.engine, initial.harness_model)).0
});

/// Changes to the installed chat engine and model, after subscription.
pub fn subscribe_chat_engine_changes() -> tokio::sync::watch::Receiver<(String, String)> {
    CHAT_ENGINE_CHANGES.subscribe()
}

/// Install / reload the chat-turn snapshot (`POST /settings/magician-config/reload`).
pub fn install_chat_harness_snapshot(mut snapshot: ChatHarnessSnapshot) {
    snapshot.engine = coerce_chat_mouth_engine(&snapshot.engine).to_string();
    let announced = (snapshot.engine.clone(), snapshot.harness_model.clone());
    *CHAT_SNAPSHOT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot;
    CHAT_ENGINE_CHANGES.send_if_modified(|current| {
        if *current == announced {
            return false;
        }
        *current = announced;
        true
    });
}

pub fn chat_harness_snapshot() -> ChatHarnessSnapshot {
    CHAT_SNAPSHOT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The parent engine of a chat turn's background LLM operations: the chat
/// mouth, unless that is the native loop, which names no parent. Read from
/// the same snapshot the seam resolves the mouth from, so the two cannot
/// disagree.
pub(crate) fn chat_turn_parent_engine() -> Option<String> {
    crate::magician_v2::query_analysis::parent_engine::normalize_parent_engine(Some(
        &chat_harness_snapshot().engine,
    ))
}

/// The pin a run launched from this chat turn inherits: the chat's mouth —
/// the composer's pick, else `chat.harness_engine` — with its model, and for
/// a Pi mouth the composer's profile, else the run Settings' Pi profile. A
/// harness mouth whose CLI is not installed answers as Magician, so a run it
/// launches does too rather than inheriting an engine that cannot start.
pub fn chat_turn_run_pin(choice: Option<&ChatHarnessChoice>) -> RunEnginePin {
    let snapshot = chat_harness_snapshot();
    let (engine, harness_model) = match choice {
        Some(choice) => (
            coerce_chat_mouth_engine(&choice.engine),
            choice.model.clone(),
        ),
        None => (
            coerce_chat_mouth_engine(&snapshot.engine),
            snapshot.harness_model.clone(),
        ),
    };
    if engine == "magician" || !chat_engine_installed(engine) {
        return RunEnginePin {
            engine: "magician".to_string(),
            harness_model: "default".to_string(),
            pi_profile: None,
        };
    }
    let pi_profile = (engine == "pi")
        .then(|| {
            choice
                .and_then(|choice| choice.profile.clone())
                .or_else(|| {
                    crate::magician_v2::execution::plane::harness_engine_snapshot().pi_profile
                })
        })
        .flatten();
    RunEnginePin {
        engine: engine.to_string(),
        harness_model: Some(harness_model)
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| "default".to_string()),
        pi_profile,
    }
}

#[derive(Default, Clone, Debug)]
struct ChatHarnessContinuation {
    native_session_id: Option<String>,
    grant_token: Option<String>,
    engine: Option<String>,
    model_fingerprint: Option<String>,
    /// See `HarnessSessionRequest::native_home`. Removed with the continuation.
    native_home: Option<PathBuf>,
}

static CHAT_CONTINUATIONS: Lazy<RwLock<HashMap<String, ChatHarnessContinuation>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));
static CHAT_GENERATIONS: Lazy<RwLock<HashMap<String, u64>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Where per-conversation native homes live. Continuations are in-memory, but
/// an older process may still own a home during an overlapping restart. Do
/// not sweep the shared root when a new process first uses it. A test process
/// never resolves the live root: under the harness the process runtime root
/// can still be the operator's (`MAGICIAN_ROOT_DIR` in the shell).
static NATIVE_HOMES_ROOT: Lazy<PathBuf> = Lazy::new(|| {
    let root = if crate::magician_v2::artifact_v2::workspace::running_under_cargo_test_harness() {
        std::env::temp_dir()
            .join(format!("magician-cargo-test-{}", std::process::id()))
            .join("plane-homes")
    } else {
        crate::magician_v2::process_storage::runtime_root()
            .join(".magician-storage")
            .join("plane")
            .join("homes")
    };
    prepare_native_homes_root(&root);
    root
});

fn prepare_native_homes_root(root: &Path) {
    if let Err(error) = std::fs::create_dir_all(root) {
        // Every mint under it fails too: the mouth degrades to cold turns.
        tracing::warn!(root = %root.display(), %error, "native homes root could not be created");
    }
    chmod_private_dir(root);
}

fn native_homes_root() -> &'static Path {
    &NATIVE_HOMES_ROOT
}

/// A fresh, unpredictable, private home under the root. `None` when the
/// filesystem refused: the turn then runs cold on a temp home, as before.
fn mint_native_home() -> Option<PathBuf> {
    let home = native_homes_root().join(uuid::Uuid::new_v4().simple().to_string());
    if let Err(error) = std::fs::create_dir_all(&home) {
        tracing::warn!(
            home = %home.display(),
            %error,
            "native home could not be minted; this turn runs cold on a temp home"
        );
        return None;
    }
    chmod_private_dir(&home);
    Some(home)
}

/// Remove a home the continuation owned. Only a direct child of the root
/// goes — a continuation never carries another path, a temp home is the
/// session's, and the root itself is never removed through here.
fn remove_native_home(home: Option<PathBuf>) {
    let Some(home) = home.filter(|home| home.parent() == Some(native_homes_root())) else {
        return;
    };
    if let Err(error) = std::fs::remove_dir_all(&home) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(home = %home.display(), %error, "native home could not be removed");
        }
    }
}

/// A newly minted home belongs to the in-flight turn until its continuation
/// is stored. If the future is aborted first, remove only that new home.
struct RemoveNewNativeHomeOnDrop(Option<PathBuf>);

impl RemoveNewNativeHomeOnDrop {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for RemoveNewNativeHomeOnDrop {
    fn drop(&mut self) {
        remove_native_home(self.0.take());
    }
}

/// The home this turn runs in. The native home follows the continuation:
/// the same engine keeps it, an engine switch or a missing one mints a fresh
/// home (the old one goes with its engine); an engine that cannot resume gets
/// none (a temp home per session). The Magician mouth and session teardown
/// remove it (abandon/forget).
fn native_home_for_turn(
    prior: &ChatHarnessContinuation,
    engine_name: &str,
    supports_resume: bool,
) -> Option<PathBuf> {
    if !supports_resume {
        remove_native_home(prior.native_home.clone());
        return None;
    }
    match prior
        .native_home
        .clone()
        .filter(|_| prior.engine.as_deref() == Some(engine_name))
    {
        Some(home) if home.is_dir() => Some(home),
        _ => {
            remove_native_home(prior.native_home.clone());
            mint_native_home()
        },
    }
}

/// The native id the continuation keeps after a turn. A refused turn keeps
/// only a thread the engine itself reported: re-storing the seeded id after
/// a failed resume would refuse every later turn the same way (a CLI resume
/// has no fallback), whereas dropping it costs one cold turn with history
/// replayed. Every other stop falls back to the seeded id.
fn native_id_to_keep(
    settled: &HarnessTurnSettled,
    resume_session_id: Option<&str>,
    supports_resume: bool,
) -> Option<String> {
    if !supports_resume {
        return None;
    }
    let reported = settled
        .native_session_id
        .clone()
        .filter(|id| !id.is_empty());
    match settled.stop_reason {
        HarnessStopReason::Refused => reported,
        _ => reported.or_else(|| resume_session_id.map(str::to_string)),
    }
}

fn conversation_generation(session_id: &str) -> u64 {
    CHAT_GENERATIONS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .copied()
        .unwrap_or(0)
}

fn bump_conversation_generation(session_id: &str) {
    let mut generations = CHAT_GENERATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let generation = generations.entry(session_id.to_string()).or_insert(0);
    *generation = generation.saturating_add(1);
}

fn continuation(session_id: &str) -> ChatHarnessContinuation {
    CHAT_CONTINUATIONS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .cloned()
        .unwrap_or_default()
}

fn store_continuation(session_id: &str, expected_generation: u64, state: ChatHarnessContinuation) {
    // Generations lock first, then continuations — same order as forget
    // and abandon, so a late store cannot reinsert after a tombstone.
    let generations = CHAT_GENERATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current = generations.get(session_id).copied().unwrap_or(0);
    if current != expected_generation {
        // The tombstone won: this state is the only reference to a home the
        // turn minted after the conversation was forgotten or taken over.
        remove_native_home(state.native_home);
        return;
    }
    CHAT_CONTINUATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(session_id.to_string(), state);
}

fn skip_chat_harness() -> Result<Option<ChatHarnessTurnOutcome>> {
    Ok(None)
}

fn abandon_for_magician_mouth(invocation: &AgentInvocationContext) {
    if let Some(session_id) = invocation
        .chat_session_id
        .as_deref()
        .filter(|id| !id.is_empty())
    {
        abandon_chat_harness_resume(session_id);
    }
}

/// A Magician-mouth turn on this conversation must not leave a roster
/// `--resume` id. Bump the generation (same tombstone as forget) so a
/// late harness `store_continuation` cannot restore it. Voice/lane skips
/// must not call this — they never thought with the native session.
///
/// The chat service also calls it after a harness turn that bridged a
/// persona change (`ChatHarnessTurnOutcome::changed_persona`): the warm
/// resume holds the old system prompt, so the next turn must be cold.
pub fn abandon_chat_harness_resume(session_id: &str) {
    // Always bump: a first harness turn may still be in flight with no
    // continuation stored yet. Skipping the bump would let its late store
    // restore --resume after this Magician mouth.
    bump_conversation_generation(session_id);
    let previous = CHAT_CONTINUATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
    forget_continuation_state(previous);
}

/// What a dropped continuation leaves behind: its retained grant (revoked
/// on the runtime, when there is one) and its native home (removed now —
/// the CLI's persisted session must not outlive the conversation's thread).
fn forget_continuation_state(previous: Option<ChatHarnessContinuation>) {
    let Some(previous) = previous else {
        return;
    };
    remove_native_home(previous.native_home);
    if let Some(token) = previous.grant_token {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                plane_grant_registry().revoke(&token).await;
            });
        }
    }
}

fn chat_engine_installed(name: &str) -> bool {
    crate::magician_v2::execution::plane::engines::roster_with_install_status()
        .into_iter()
        .any(|(engine, installed)| engine == name && installed)
}

/// Drop the conversation's warm native session and revoke any retained grant.
/// Called from chat session teardown so a deleted conversation cannot resume.
pub fn forget_chat_harness_conversation(session_id: &str) {
    bump_conversation_generation(session_id);
    let previous = CHAT_CONTINUATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
    forget_continuation_state(previous);
}

pub struct ChatHarnessTurnRequest<'a> {
    pub invocation: &'a AgentInvocationContext,
    pub system_prompt: &'a str,
    pub history: &'a [ChatLlmTranscriptEntry],
    pub user_text: &'a str,
    pub cancel: CancellationToken,
    pub disclosure_guarded: bool,
    pub token_sink: Option<mpsc::Sender<magicllm::StreamDelta>>,
    pub approval_rules: &'a [ApprovalRule],
    /// The turn's resolved policy. `None` skips the harness (a swapped mouth
    /// must never see "the whole plane catalog").
    pub policy_snapshot: Option<&'a EffectiveToolPolicySnapshot>,
    /// Plane posture of every chat-runtime tool (`plane_posture_index`).
    pub plane_postures: &'a HashMap<String, PlanePosture>,
    /// The turn's scoped tool index, so the grant's `tool_search` can find and
    /// load the Deferred names in the allowlist. `None` leaves only the plane's
    /// hot tools callable.
    pub tool_index: Option<Arc<ToolIndex>>,
    pub trust_level: Option<&'a str>,
    pub trust_enforcer: Option<Arc<TrustPolicyEnforcer>>,
    pub trust_policies_path: Option<&'a std::path::Path>,
    /// The tool specs the native mouth's provider would receive this turn.
    /// Those the plane does not execute itself become the grant's bridged
    /// set when `mouth_bridge` is present; without a bridge none are offered.
    pub mouth_tool_specs: &'a [LLMToolSpec],
    /// The chat service's dispatcher, packaged for this turn. `None` keeps
    /// the swapped mouth to the plane-executed hands.
    pub mouth_bridge: Option<ChatMouthBridge>,
    /// Explicit composer choice for this turn. None uses Settings.
    pub choice: Option<&'a ChatHarnessChoice>,
    /// The resolved Magician chat profile used by Pi when selected.
    pub pi_profile: Option<magicllm::config::LlmConfig>,
    /// Current prompt images: Pi RPC, or the restricted planner image channel.
    pub pi_images: Vec<serde_json::Value>,
}

const CHAT_HARNESS_PROFILE_PREFIX: &str = "__magician_chat_harness_v1__";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatHarnessChoice {
    pub engine: String,
    pub model: String,
    pub profile: Option<String>,
}

/// A client's chat engine choice, with the engine checked the way a typed
/// turn's is: a roster engine that is installed here (or `magician`), and a
/// model name that is short and printable. The profile is only checked for
/// shape; one the catalog does not list falls through to the chat service's
/// own profile fallback rather than failing the call. A voice call carries the choice its client's
/// composer holds, so a spoken turn thinks with the same engine as a typed one.
pub fn validated_client_chat_harness_choice(
    engine: &str,
    model: Option<&str>,
    profile: Option<&str>,
) -> std::result::Result<ChatHarnessChoice, String> {
    let engine = engine.trim();
    let coerced = coerce_chat_mouth_engine(engine);
    if coerced != engine {
        return Err(format!("Chat harness '{engine}' is not a chat engine"));
    }
    if engine != "magician" && !chat_engine_installed(engine) {
        return Err(format!("Chat harness '{engine}' is not installed"));
    }
    let model = model.map(str::trim).filter(|model| !model.is_empty());
    if model.is_some_and(|model| model.len() > 128 || model.chars().any(char::is_control)) {
        return Err("Chat harness model is invalid".to_string());
    }
    let profile = profile.map(str::trim).filter(|profile| !profile.is_empty());
    if profile.is_some_and(|profile| profile.len() > 128 || profile.chars().any(char::is_control)) {
        return Err("Chat profile is invalid".to_string());
    }
    Ok(ChatHarnessChoice {
        engine: engine.to_string(),
        model: model.unwrap_or("default").to_string(),
        profile: profile.map(str::to_string),
    })
}

/// Keep a queued turn's exact route inside the existing profile override slot.
/// The public API accepts separate fields; this representation is internal.
pub fn encode_chat_harness_choice(choice: &ChatHarnessChoice) -> String {
    format!(
        "{CHAT_HARNESS_PROFILE_PREFIX}{}",
        serde_json::to_string(choice).expect("chat harness choice serializes")
    )
}

pub fn decode_chat_harness_choice(profile: Option<&str>) -> Option<ChatHarnessChoice> {
    profile
        .and_then(|value| value.strip_prefix(CHAT_HARNESS_PROFILE_PREFIX))
        .and_then(|value| serde_json::from_str(value).ok())
}

/// A harness turn that occupied the mouth: how it settled, and every plane
/// call it dispatched, for the chat transcript. A turn that failed after
/// some calls still carries them — the transcript records what ran, not
/// only what replied.
#[derive(Debug)]
pub struct ChatHarnessTurnOutcome {
    pub decision_model_calls: Vec<decision_engine_contract::telemetry::DecisionModelCall>,
    pub settled: HarnessTurnSettled,
    pub tool_calls: Vec<PlaneTurnToolCall>,
}

/// Bridged names whose effect rewrites the conversation's system prompt.
/// A warm harness resume would keep thinking under the old one, so a turn
/// in which one of these landed ends the resume
/// (`abandon_chat_harness_resume`) and the next turn starts cold.
pub const PERSONA_CHANGING_TOOLS: &[&str] =
    &["switch_personality", "activate_skill", "deactivate_skill"];

impl ChatHarnessTurnOutcome {
    /// Whether a call this turn dispatched switched the persona: one of
    /// `PERSONA_CHANGING_TOOLS` whose recorded result says the switch
    /// landed (`persona_switch_landed`). Decided from the result, not the
    /// name alone: the switch tool's list mode changes nothing, and every
    /// error path in the three handlers returns before the memory write,
    /// while a false positive both ends the warm resume and removes the
    /// native home, which nothing restores.
    pub fn changed_persona(&self) -> bool {
        self.tool_calls.iter().any(|call| {
            PERSONA_CHANGING_TOOLS.contains(&call.tool_name.as_str())
                && !call.is_error
                && persona_switch_landed(&call.content)
        })
    }
}

/// Whether a persona tool's recorded result is a switch that landed: the
/// handler's own success object (`status` ok) minus its no-change answers
/// — `switch_personality`'s list mode (`action` list), `activate_skill`
/// re-activating the skill already active (`outcome` idempotent),
/// `deactivate_skill` with nothing active (`outcome` noop) — and, for a call
/// the native dispatcher routed through its executor sub-run (whose folded
/// envelope reports `status` ok for every terminal outcome), its `success`
/// flag. A record that is not JSON (cut by the ledger bound) is not a
/// switch.
fn persona_switch_landed(content: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return false;
    };
    value.get("status").and_then(serde_json::Value::as_str) == Some("ok")
        && value.get("action").and_then(serde_json::Value::as_str) != Some("list")
        && !matches!(
            value.get("outcome").and_then(serde_json::Value::as_str),
            Some("idempotent" | "noop")
        )
        && value.get("success").and_then(serde_json::Value::as_bool) != Some(false)
}

/// Calls per transcript append when a harness turn's ledger is persisted.
/// Each append carries one assistant turn naming its calls and one result
/// per call, so this keeps every append inside the store's per-append entry
/// and open-call caps whatever the turn's call budget was.
pub const HARNESS_TRANSCRIPT_CHUNK_CALLS: usize = 200;

/// The transcript entries a harness turn's calls persist as — the shape the
/// native mouth writes — in batches of at most `chunk` calls: each batch is
/// one `AssistantTurn` naming its calls followed by one `ToolResult` per
/// call, balanced on its own so a batch that fails to append leaves no
/// call open, and dispatch order is kept across batches.
pub fn harness_transcript_batches(
    calls: &[PlaneTurnToolCall],
    chunk: usize,
) -> Vec<Vec<ChatLlmTranscriptEntry>> {
    calls
        .chunks(chunk.max(1))
        .map(|batch| {
            let mut entries = Vec::with_capacity(batch.len() + 1);
            entries.push(ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: batch
                    .iter()
                    .map(|call| StoredToolCall {
                        id: call.call_id.clone(),
                        name: call.tool_name.clone(),
                        arguments: call.arguments.clone(),
                    })
                    .collect(),
                provider_state: None,
            });
            entries.extend(batch.iter().map(|call| ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: call.call_id.clone(),
                tool_name: Some(call.tool_name.clone()),
                content: call.content.clone(),
            }));
            entries
        })
        .collect()
}

fn chat_model_fingerprint(
    engine_name: &str,
    harness_model: &str,
    pi_profile: Option<&magicllm::config::LlmConfig>,
) -> String {
    if engine_name != "pi" {
        return harness_model.to_string();
    }
    // A resumed Pi session can restore its previous thinking level and model
    // metadata. Include every profile field installed in its private model
    // entry so editing a profile starts a fresh session with the new values.
    serde_json::json!({
        "harness_model": harness_model,
        "profile": pi_profile.map(|profile| serde_json::json!({
            "provider": profile.provider.as_str(),
            "model": profile.model,
            "api_base_url": profile.api_base_url,
            "api_key_env": profile.api_key_env,
            "reasoning_effort": profile.reasoning_effort,
            "supports_reasoning": profile.supports_reasoning,
            "supports_vision": profile.supports_vision,
            "max_tokens": profile.max_tokens,
        })),
    })
    .to_string()
}

/// Run a harness mouth in place of the Magician LLM call when configured.
///
/// Returns `Ok(None)` so today's path proceeds (default, unknown engine,
/// protected conversation, meeting / Tutor / App Copilot / envoy).
/// Live/Realtime `delegate_to_chat` uses `InvocationSurface::RealtimeVoice`
/// and follows `chat.harness_engine` so Live can be the mouth for Magician
/// or another roster harness.
pub async fn maybe_harness_chat_turn(
    request: ChatHarnessTurnRequest<'_>,
) -> Result<Option<ChatHarnessTurnOutcome>> {
    if request.disclosure_guarded {
        abandon_for_magician_mouth(request.invocation);
        return skip_chat_harness();
    }
    if matches!(
        request.invocation.surface,
        InvocationSurface::Meeting
            | InvocationSurface::AppCopilot
            | InvocationSurface::PublicEnvoy
            | InvocationSurface::Tutor
    ) || matches!(
        request.invocation.feature_mode,
        FeatureMode::AppCopilot | FeatureMode::Tutor
    ) {
        return skip_chat_harness();
    }
    let mut snapshot = chat_harness_snapshot();
    if let Some(choice) = request.choice {
        snapshot.engine = choice.engine.clone();
        snapshot.harness_model = choice.model.clone();
    }
    let TurnEngine::Harness(engine_name) = resolve_turn_engine(Some(&snapshot.engine)) else {
        abandon_for_magician_mouth(request.invocation);
        return skip_chat_harness();
    };
    let allowed_tools = request
        .policy_snapshot
        .map(|policy| plane_hands_allowlist(policy, request.plane_postures))
        .unwrap_or_default();
    if allowed_tools.is_empty() {
        tracing::debug!(
            engine = engine_name,
            reason = if request.policy_snapshot.is_none() {
                "no_policy_snapshot"
            } else {
                "no_plane_hands"
            },
            "chat harness skipped; the Magician mouth answers this turn"
        );
        abandon_for_magician_mouth(request.invocation);
        return skip_chat_harness();
    }

    let Some(session_id) = request
        .invocation
        .chat_session_id
        .as_deref()
        .filter(|id| !id.is_empty())
    else {
        return Ok(None);
    };
    if !chat_engine_installed(engine_name) {
        abandon_for_magician_mouth(request.invocation);
        return skip_chat_harness();
    }
    let model_fingerprint = chat_model_fingerprint(
        engine_name,
        &snapshot.harness_model,
        request.pi_profile.as_ref(),
    );
    let generation = conversation_generation(session_id);
    let prior = continuation(session_id);
    if let Some(token) = &prior.grant_token {
        plane_grant_registry().revoke(token).await;
    }

    let Some(engine) =
        crate::magician_v2::execution::plane::engines::harness_engine_for(engine_name)
    else {
        abandon_for_magician_mouth(request.invocation);
        return skip_chat_harness();
    };
    let supports_resume = engine.capabilities().supports_resume;
    let native_home = native_home_for_turn(&prior, engine_name, supports_resume);
    let mut new_home_lease = RemoveNewNativeHomeOnDrop(
        native_home
            .as_ref()
            .filter(|home| prior.native_home.as_ref() != Some(home))
            .cloned(),
    );
    // A native id is only good with the home it was minted alongside: a
    // re-minted home (engine switch, or a home that vanished) holds no
    // session to resume, and a CLI's resume has no fallback to a cold start.
    let resume_session_id = if supports_resume
        && prior.engine.as_deref() == Some(engine_name)
        && prior.model_fingerprint.as_deref() == Some(model_fingerprint.as_str())
        && native_home == prior.native_home
    {
        prior.native_session_id.clone().filter(|id| !id.is_empty())
    } else {
        None
    };

    let mut ctx = AgenticContext::default();
    ctx.principal = Some(request.invocation.principal.clone());
    ctx.workspace = Some(request.invocation.workspace.clone());
    ctx.agent_id = Some(request.invocation.target_agent_id.clone());
    ctx.chat_session_id = request.invocation.chat_session_id.clone();
    ctx.invocation_context_override = Some(request.invocation.clone());
    ctx.harness_engine = Some(engine_name.to_string());
    // A run this turn's hands launch inherits the mouth: the door scopes the
    // grant's pin around each call as the launching run's pin.
    ctx.run_engine_pin = Some(chat_turn_run_pin(request.choice));
    if let Some(level) = request.trust_level {
        ctx.trust_level = Some(level.to_string());
    }
    ctx.preloaded_trust_enforcer = request.trust_enforcer.clone();
    ctx.trust_policies_path = request
        .trust_policies_path
        .map(std::path::Path::to_path_buf);
    ctx.approval_rules = request.approval_rules.to_vec();

    let mut grant =
        PlaneGrant::for_conversation(ctx, session_id.to_string(), request.cancel.clone())
            .with_turn_tool_budget(snapshot.turn_max_tool_calls, 0);
    if let Some(index) = request.tool_index.clone() {
        grant = grant.with_tool_index(index);
    }
    grant.allowed_tools = allowed_tools;
    let preloaded = preload_deferred_hands(&mut grant, engine.capabilities().tools_list_changed);
    if preloaded > 0 {
        tracing::debug!(
            engine = engine_name,
            preloaded,
            "deferred hands loaded at mint; this mouth does not re-list tools mid-turn"
        );
    }
    grant.constraints.requires_approval = request.approval_rules.to_vec();
    if grant.executors.is_none() {
        grant.executors = scoped_runless_executors(&grant.ctx, &grant.allowed_tools);
    }
    let grant = bridge_mouth_tools(
        grant,
        request.mouth_tool_specs,
        request.mouth_bridge.clone(),
        request.plane_postures,
        request.policy_snapshot,
        engine_name,
    );
    match crate::magician_v2::chat::decision_rail::available_scoped(
        engine_name,
        &request.cancel,
        &request.invocation.principal,
        &request.invocation.workspace,
    )
    .await
    {
        Ok(Some(client)) => {
            // Restricted proposal rounds cannot resume a CLI session that had
            // direct work authority. The host transcript is the continuation.
            abandon_chat_harness_resume(session_id);
            return Ok(Some(
                super::chat_decision_rail::run(&request, &snapshot, engine_name, grant, client)
                    .await,
            ));
        },
        Ok(None) => {},
        Err(error) => {
            return Ok(Some(ChatHarnessTurnOutcome {
                decision_model_calls: Vec::new(),
                settled: HarnessTurnSettled {
                    assistant_text: format!("The chat turn stopped: {error}"),
                    stop_reason: HarnessStopReason::Refused,
                    usage: None,
                    native_session_id: None,
                },
                tool_calls: Vec::new(),
            }))
        },
    }
    let ledger = grant.turn_ledger.clone();
    let token = plane_grant_registry().mint_chat(grant).await;
    let mut grant_lease = RevokeGrantOnDrop::new(token.clone());

    let (sink, mut delta_rx) = HarnessStreamSink::channel();
    let token_sink = request.token_sink.clone();
    let streamed = Arc::new(AtomicBool::new(false));
    let streamed_flag = Arc::clone(&streamed);
    let collected = Arc::new(std::sync::Mutex::new(String::new()));
    let collected_for_pump = Arc::clone(&collected);
    let pump = tokio::spawn(async move {
        while let Some(delta) = delta_rx.recv().await {
            collected_for_pump
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push_str(&delta);
            if let Some(tx) = token_sink.as_ref() {
                if tx.try_send(magicllm::StreamDelta::Token(delta)).is_ok() {
                    streamed_flag.store(true, Ordering::Relaxed);
                }
            }
        }
    });

    let req = HarnessSessionRequest {
        planning_only: false,
        turn_idle_timeout: Some(std::time::Duration::from_secs(
            crate::config::default_harness_turn_idle_seconds(),
        )),
        endpoint: PlaneEndpoint {
            url: snapshot.plane_endpoint.clone(),
        },
        grant: token.clone(),
        // Provider-bound: the same redaction Magician's own model call gets.
        system_prompt: sanitize_text_for_provider(request.system_prompt),
        model: Some(snapshot.harness_model.clone()).filter(|m| m != "default"),
        pi_profile: request.pi_profile.clone(),
        pi_images: if engine_name == "pi" {
            request.pi_images.clone()
        } else {
            Vec::new()
        },
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        env_allowlist: Vec::new(),
        cancel: Some(request.cancel.clone()),
        // Cloned into the request: the original feeds the turn-input text
        // composition below, which needs it after this move.
        resume_session_id: resume_session_id.clone(),
        turn_timeout: Duration::from_secs(snapshot.turn_max_seconds),
        native_home: native_home.clone(),
    };
    let mut settled_health = None;
    let turn_result = async {
        let mut session = engine.start(&req).await?;
        let mut prompt_text = turn_input_text(
            request.history,
            request.user_text,
            request.system_prompt,
            engine_name,
            resume_session_id.as_deref(),
        );
        if engine_name == "pi" && !request.pi_images.is_empty() {
            const OMITTED: &str = "[attachment omitted from harness prompt]";
            if let Some(start) = prompt_text.rfind(OMITTED) {
                prompt_text.replace_range(start..start + OMITTED.len(), "[image attached]");
            }
        }
        let settled = session
            .turn(
                &HarnessTurnInput {
                    text: prompt_text,
                    operator_steer: Vec::new(),
                },
                &sink,
            )
            .await?;
        settled_health = session.service_health(&settled);
        session.shutdown().await;
        Ok::<HarnessTurnSettled, crate::magician_v2::execution::plane::engine::HarnessError>(
            settled,
        )
    }
    .await;
    drop(sink);
    let _ = pump.await;

    if !request.cancel.is_cancelled() {
        let health = match &turn_result {
            Ok(_) => settled_health,
            Err(error) => {
                crate::magician_v2::realtime_events::ServiceFailure::from_error(&error.to_string())
                    .map(Err)
            },
        };
        if let Some(health) = health {
            crate::magician_v2::decision_host::report_health(
                &request.invocation.principal,
                &request.invocation.workspace,
                &req.health_service(engine_name),
                health,
            );
        }
    }
    let settled = match turn_result {
        Ok(settled) => settled,
        Err(error) => {
            // The home stays: a spawn failure is not a reason to lose the
            // thread the CLI persisted in it.
            store_continuation(
                session_id,
                generation,
                ChatHarnessContinuation {
                    native_session_id: None,
                    grant_token: None,
                    engine: Some(engine_name.to_string()),
                    model_fingerprint: Some(model_fingerprint.clone()),
                    native_home,
                },
            );
            new_home_lease.disarm();
            plane_grant_registry().revoke(&token).await;
            grant_lease.disarm();
            let collected_text = collected
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            // Grant was minted: do not fall through to Magician. Spawn
            // failures and mid-turn crashes both already occupied the mouth.
            let assistant_text = if !collected_text.is_empty() {
                collected_text
            } else if streamed.load(Ordering::Relaxed) {
                "The harness turn failed after it started speaking.".to_string()
            } else {
                format!("The harness turn failed: {error}")
            };
            return Ok(Some(ChatHarnessTurnOutcome {
                decision_model_calls: Vec::new(),
                settled: HarnessTurnSettled {
                    assistant_text,
                    stop_reason: HarnessStopReason::Refused,
                    usage: None,
                    native_session_id: None,
                },
                // A turn that died mid-way still ran these.
                tool_calls: drain_turn_ledger(&ledger),
            }));
        },
    };

    let native_session_id =
        native_id_to_keep(&settled, resume_session_id.as_deref(), supports_resume);
    // Chat HITL is the next user message. Never retain the grant for
    // NeedsApproval — shutdown already released it, and EndTurn pause is
    // a run-loop mechanism this surface does not have.
    let cancelled = matches!(settled.stop_reason, HarnessStopReason::Cancelled)
        || request.cancel.is_cancelled();
    store_continuation(
        session_id,
        generation,
        ChatHarnessContinuation {
            native_session_id: if cancelled { None } else { native_session_id },
            grant_token: None,
            engine: Some(engine_name.to_string()),
            model_fingerprint: Some(model_fingerprint),
            // Kept on cancel too: a cancelled turn can still resume.
            native_home,
        },
    );
    new_home_lease.disarm();
    plane_grant_registry().revoke(&token).await;
    grant_lease.disarm();
    // Drained after revoke, so no call authorized from here on can land on
    // this grant. A call already past the door when the grant was revoked
    // may still be executing; it finishes on the revoked grant and is not
    // in this drain.
    let tool_calls = drain_turn_ledger(&ledger);

    if cancelled {
        return Ok(Some(ChatHarnessTurnOutcome {
            decision_model_calls: Vec::new(),
            settled,
            tool_calls,
        }));
    }

    let collected_text = collected
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let mut settled = settled;
    let assistant_text =
        reply_to_persist(std::mem::take(&mut settled.assistant_text), &collected_text);
    // A turn that streamed nothing to the client shows its reply live as
    // one chunk — the engine does not stream, or the CLI refused before any
    // text, or an older CLI has no delta mode. Whether anything streamed is
    // the gate, not the roster's `streams_text_deltas` promise.
    if !streamed.load(Ordering::Relaxed) {
        if let Some(tx) = request.token_sink.as_ref() {
            if !assistant_text.is_empty() {
                let _ = tx.try_send(magicllm::StreamDelta::Token(assistant_text.clone()));
            }
        }
    }
    let assistant_text = if assistant_text.is_empty() {
        "The harness turn ended without a reply.".to_string()
    } else {
        assistant_text
    };
    Ok(Some(ChatHarnessTurnOutcome {
        decision_model_calls: Vec::new(),
        settled: HarnessTurnSettled {
            assistant_text,
            ..settled
        },
        tool_calls,
    }))
}

/// The reply the turn persists. A turn that streamed is persisted from the
/// deltas the chat side collected — whole, where the engine's settled text
/// is bounded and ends its head in `PLANE_TURN_RESULT_CUT_MARK` — and, on a
/// failing exit, keeps the CLI's final error text the engine appended after
/// the streamed text (past the cut mark when the engine cut). A turn that
/// streamed nothing (or only whitespace) keeps the engine's settled text.
fn reply_to_persist(settled_text: String, collected: &str) -> String {
    if collected.trim().is_empty() {
        return settled_text;
    }
    if settled_text.starts_with(collected) {
        // Nothing was cut: the engine's text is every delta, plus whatever
        // a failing exit appended after them.
        return settled_text;
    }
    let mut text = collected.to_string();
    if let Some((_, appended)) = settled_text.split_once(PLANE_TURN_RESULT_CUT_MARK) {
        text.push_str(appended);
    }
    text
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn harness_chat_usage_event(
    trace: magicllm::LlmTraceContext,
    engine: &str,
    agent: &str,
    profile: Option<String>,
    settled: &HarnessTurnSettled,
    started_at_ms: i64,
    timestamp: i64,
    latency_ms: u64,
) -> crate::magician_v2::realtime_events::RuntimeTransportEvent {
    use crate::magician_v2::realtime_events::{LlmEventCorrelation, RuntimeTransportEvent};
    let session = trace.chat_session_id.clone();
    let execution_id = trace.execution_id.clone().unwrap_or_default();
    let principal = trace.scope.principal.clone();
    let workspace = trace.scope.workspace.clone();
    let receipt = magicllm::LlmTraceReceipt::direct_with_attempt_count(trace, 0);
    let mut correlation = LlmEventCorrelation::from(&receipt);
    correlation.usage_availability = Some(
        settled
            .usage
            .as_ref()
            .map(HarnessUsage::availability)
            .unwrap_or_default(),
    );
    let (input_tokens, output_tokens, cache_read_tokens) =
        harness_usage_to_chat_tokens(settled.usage.as_ref());
    let failed = matches!(
        settled.stop_reason,
        HarnessStopReason::Cancelled
            | HarnessStopReason::Refused
            | HarnessStopReason::TurnBudgetSpent
            | HarnessStopReason::Stalled
    );
    RuntimeTransportEvent::LLMResponseReceived {
        execution_id,
        principal: Some(principal),
        workspace: Some(workspace),
        correlation: Some(correlation),
        plan_id: String::new(),
        step_id: None,
        step_index: None,
        capability: "chat.inline".into(),
        success: !failed,
        decision_summary: String::new(),
        cost: settled
            .usage
            .as_ref()
            .and_then(|u| u.cost_usd)
            .unwrap_or_default(),
        latency_ms,
        error: failed.then(|| format!("{:?}", settled.stop_reason)),
        provider: format!("harness:{engine}"),
        model: settled
            .usage
            .as_ref()
            .and_then(|u| u.model.clone())
            .unwrap_or_default(),
        usage_reported: settled.usage.is_some(),
        input_tokens,
        output_tokens,
        reasoning_tokens: 0,
        reasoning_summary: None,
        cache_read_tokens,
        cache_creation_tokens: settled
            .usage
            .as_ref()
            .and_then(|u| u.cache_creation_tokens)
            .unwrap_or_default()
            .min(u32::MAX as u64) as u32,
        audio_input_tokens: None,
        audio_output_tokens: None,
        audio_cached_tokens: None,
        search_calls: 0,
        ttft_ms: None,
        task_id: None,
        agent_id: Some(agent.into()),
        delegated_agent_id: None,
        chat_session_id: session,
        operation: "chat_inline".into(),
        profile,
        attempt: 0,
        response_kind: "harness_aggregate".into(),
        started_at_ms,
        timestamp,
    }
}

pub fn harness_usage_to_chat_tokens(usage: Option<&HarnessUsage>) -> (u32, u32, u32) {
    let Some(usage) = usage else {
        return (0, 0, 0);
    };
    (
        u32::try_from(usage.input_tokens).unwrap_or(u32::MAX),
        u32::try_from(usage.output_tokens).unwrap_or(u32::MAX),
        u32::try_from(usage.cached_input_tokens.min(usage.input_tokens)).unwrap_or(u32::MAX),
    )
}

/// Names the plane's own executors never run for a swapped chat mouth.
/// They are not withheld: each reaches the native mouth's implementation
/// through the mouth bridge (`plane_bridged_specs`), which runs it with the
/// turn's exact chat context — the personality and skill switches act on
/// the conversation, and the task spawns carry the turn's identity and
/// approval rules. Executed on the plane instead, the switches would land
/// on no conversation and the spawns would start an unattenuated Magician
/// run (the hole `run_task` was closed for).
const NOT_A_PLANE_EXECUTED_HAND: &[&str] = &[
    "switch_personality",
    "activate_skill",
    "deactivate_skill",
    "create_task",
    "run_task",
];

/// The floor for what a swapped chat mouth may execute on the plane itself:
/// names that never enter the plane-executed allowlist, whatever the turn's
/// policy says. One predicate so the grant builder and the chat-runtime
/// posture test cannot disagree about it.
pub(crate) fn chat_harness_floor_rejects(name: &str) -> bool {
    NEVER_ON_THE_PLANE.contains(&name)
        || crate::magician_v2::execution::plane::PLANE_CONTROL_VERBS.contains(&name)
        || NOT_A_PLANE_EXECUTED_HAND.contains(&name)
        || crate::magician_v2::execution::compiled_dispatch::is_governed_app_compiled_tool(name)
}

/// The door's own verbs: answered by the plane itself before any dispatch,
/// under the plane's own schema. Never bridged, whatever the native mouth
/// advertises under the name, or the mouth's entry would shadow the door's.
const PLANE_DOOR_VERBS: &[&str] = &["tool_search", "session_ledger"];

/// Verbs the native loop answers itself, before dispatch: the model issues
/// the call, the loop intercepts it by name, and `dispatch_chat_tool_call`
/// never sees it — an adaptive profile's thinking-mode escalation is one.
/// The native mouth advertises them in its specs like any tool, but through
/// the bridge there is no loop in front of the dispatcher: the name would
/// fall to the dispatcher's pack fallthrough and start a task for a pack
/// that does not exist. Never bridged.
const LOOP_INTERCEPTED_VERBS: &[&str] =
    &[crate::magician_v2::chat::service::REQUEST_THINKING_MODE_TOOL_NAME];

/// Names the native mouth advertises that the plane does not execute itself
/// and that may cross through the mouth bridge: everything the plane-executed
/// floor rejects EXCEPT the nesting floor, the door's own verbs, the loop's
/// own intercepted verbs, and governed app tools. Of the loop's control
/// verbs only `read_result` crosses — the native mouth has a `read_result`
/// of its own; the rest are how Magician's loop yields.
pub(crate) fn chat_harness_bridgeable(name: &str) -> bool {
    !NEVER_ON_THE_PLANE.contains(&name)
        && !PLANE_DOOR_VERBS.contains(&name)
        && !LOOP_INTERCEPTED_VERBS.contains(&name)
        && !crate::magician_v2::execution::compiled_dispatch::is_governed_app_compiled_tool(name)
        && (name == "read_result"
            || !crate::magician_v2::execution::plane::PLANE_CONTROL_VERBS.contains(&name))
}

/// The bridged set for one turn: the native mouth's advertised specs minus
/// what the plane executes itself (`plane_allowlist`), minus every runtime
/// tool whose posture is not `Bridged` — a `Counterpart` already crosses as
/// its plane twin, and its native name (in the mouth's specs too) would
/// hand the mouth the same tool twice; `MouthOnly` is withheld — under the
/// bridge floor. Keyed by name, so the grant can advertise and route by it.
pub(crate) fn plane_bridged_specs(
    specs: &[LLMToolSpec],
    plane_allowlist: &[String],
    postures: &HashMap<String, PlanePosture>,
) -> BTreeMap<String, LLMToolSpec> {
    specs
        .iter()
        .filter(|spec| !plane_allowlist.iter().any(|name| name == &spec.name))
        .filter(|spec| {
            !matches!(
                postures.get(&spec.name),
                Some(PlanePosture::Counterpart(_) | PlanePosture::MouthOnly)
            )
        })
        .filter(|spec| chat_harness_bridgeable(&spec.name))
        .map(|spec| (spec.name.clone(), spec.clone()))
        .collect()
}

/// The compiled names the turn's policy grants that the plane never executes
/// itself (`NOT_A_PLANE_EXECUTED_HAND`), as specs from the tool index. The
/// native mouth reaches these through its own dispatcher — a Deferred one
/// after a `tool_search` load — so they are not in the provider's hot spec
/// list, and without this they would land in neither set.
fn floor_rejected_compiled_specs(
    policy: Option<&EffectiveToolPolicySnapshot>,
    index: &ToolIndex,
) -> Vec<LLMToolSpec> {
    let Some(policy) = policy else {
        return Vec::new();
    };
    bridged_deferred_hands(policy)
        .iter()
        .filter_map(|name| index.get(name))
        .map(|entry| LLMToolSpec {
            name: entry.name.clone(),
            description: entry.description.clone(),
            parameters: entry.parameters_schema.clone(),
        })
        .collect()
}

/// The compiled names the policy grants that only the mouth bridge can run
/// for a swapped mouth (`NOT_A_PLANE_EXECUTED_HAND`), under the same two
/// exclusions the plane-executed allowlist applies. `permits_tool` is not
/// the test: it also demands a provider-callable (loaded) Deferred name,
/// which is exactly what these are not yet. The chat service widens the
/// native dispatcher's per-turn ceiling by these names, as the native
/// mouth's own `tool_search` load would.
pub fn bridged_deferred_hands(policy: &EffectiveToolPolicySnapshot) -> Vec<String> {
    NOT_A_PLANE_EXECUTED_HAND
        .iter()
        .filter(|name| {
            policy.direct_tools.contains_key(**name) || policy.deferred_tools.contains_key(**name)
        })
        .filter(|name| {
            !policy.denied_tool_names.contains(**name)
                && !policy.delegate_owned_tool_names.contains(**name)
        })
        .filter(|name| chat_harness_bridgeable(name))
        .map(|name| name.to_string())
        .collect()
}

/// The request's mouth tools, onto the grant. Runs after the executors and
/// the preload, which are scoped to the plane-executed allowlist alone: a
/// bridged name never needs a plane executor or a loaded index entry, and
/// `with_bridged_tools` widens `allowed_tools` to admit the bridged names
/// at the door. Without a bridge the grant is returned untouched — the
/// specs alone offer nothing, there would be no way to run them.
fn bridge_mouth_tools(
    grant: PlaneGrant,
    mouth_tool_specs: &[LLMToolSpec],
    mouth_bridge: Option<ChatMouthBridge>,
    postures: &HashMap<String, PlanePosture>,
    policy: Option<&EffectiveToolPolicySnapshot>,
    engine_name: &str,
) -> PlaneGrant {
    let Some(bridge) = mouth_bridge else {
        return grant;
    };
    let mut specs: Vec<LLMToolSpec> = mouth_tool_specs.to_vec();
    for spec in floor_rejected_compiled_specs(policy, grant.tool_index.as_ref()) {
        if !specs.iter().any(|known| known.name == spec.name) {
            specs.push(spec);
        }
    }
    let bridged = plane_bridged_specs(&specs, &grant.allowed_tools, postures);
    if !bridged.is_empty() {
        tracing::debug!(
            engine = engine_name,
            bridged = bridged.len(),
            "native mouth tools bridged onto the harness grant"
        );
    }
    grant.with_bridged_tools(bridged, bridge)
}

/// The allowlisted names a mouth could only reach through `tool_search`:
/// known to the scope's index but not hot for the grant's profile. A mouth
/// that cannot re-list tools mid-turn gets them loaded at mint instead.
fn deferred_hands_to_preload(
    allowed: &[String],
    index: &ToolIndex,
    profile: PlaneCatalogProfile,
) -> HashSet<String> {
    let hot = builtin_hot_names(profile);
    allowed
        .iter()
        .filter(|name| index.get(name).is_some() && !hot.contains(&name.as_str()))
        .cloned()
        .collect()
}

/// Load the grant's Deferred hands at mint when the engine never re-lists
/// tools (`relists` is its `tools_list_changed`). Returns how many were
/// loaded. The builtin hot list stands in for the profile's configured one —
/// the mint has no `PlaneConfig` in hand — so a name that is hot only by
/// overlay may be preloaded too; harmless, `tools/list` folds hot and loaded
/// into one set.
fn preload_deferred_hands(grant: &mut PlaneGrant, relists: bool) -> usize {
    if relists {
        return 0;
    }
    let preload = deferred_hands_to_preload(
        &grant.allowed_tools,
        &grant.tool_index,
        grant.catalog_profile,
    );
    if preload.is_empty() {
        return 0;
    }
    let count = preload.len();
    grant.replace_loaded_tools(preload);
    grant.preloaded_deferred = true;
    count
}

/// Plane-executed hands for a swapped chat mouth, projected from the turn's
/// resolved policy. Direct and Deferred tools cross under their own names
/// (both sides lower through the same definition store); Runtime tools cross
/// only as the counterpart names their `PlanePosture` declares (a Bridged
/// tool contributes nothing here — it rides the bridged set, not this
/// allowlist); the floor applies last. Empty must never mean "the whole
/// catalog".
fn plane_hands_allowlist(
    policy: &EffectiveToolPolicySnapshot,
    postures: &HashMap<String, PlanePosture>,
) -> Vec<String> {
    let compiled = policy
        .direct_tools
        .keys()
        .chain(policy.deferred_tools.keys())
        .cloned();
    let counterparts = policy
        .runtime_tools
        .keys()
        .filter(|name| policy.permits_tool(name))
        .flat_map(|name| match postures.get(name) {
            Some(PlanePosture::Counterpart(plane_names)) => plane_names.to_vec(),
            Some(PlanePosture::Bridged | PlanePosture::MouthOnly) | None => Vec::new(),
        })
        .map(str::to_string);
    let mut seen = std::collections::HashSet::new();
    compiled
        .chain(counterparts)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty() && seen.insert(name.clone()))
        .filter(|name| {
            !policy.denied_tool_names.contains(name)
                && !policy.delegate_owned_tool_names.contains(name)
                && !chat_harness_floor_rejects(name)
        })
        .collect()
}

/// Cold start: Magician transcript + this user message. Warm `--resume`:
/// only the current user turn, including its injected context — the native
/// session already has the older conversation.
///
/// Claude receives the persona via `--append-system-prompt` on argv. One-shot
/// engines do not, so the system prompt is prepended on a cold turn only.
pub(crate) fn turn_input_text(
    history: &[ChatLlmTranscriptEntry],
    user_text: &str,
    system_prompt: &str,
    engine_name: &str,
    native_session_id: Option<&str>,
) -> String {
    // Everything handed to a harness is provider-bound. The LLM-provider path
    // redacts before the model sees it; a swapped engine must see no more.
    // This pass covers the conversation text; each replayed tool result and
    // call was redacted on its own before it was cut (`push_tool_result`,
    // `push_tool_call`), so the two passes overlap rather than compete.
    sanitize_text_for_provider(&turn_input_text_unsanitized(
        history,
        user_text,
        system_prompt,
        engine_name,
        native_session_id,
    ))
}

fn turn_input_text_unsanitized(
    history: &[ChatLlmTranscriptEntry],
    user_text: &str,
    system_prompt: &str,
    engine_name: &str,
    native_session_id: Option<&str>,
) -> String {
    if native_session_id.is_some_and(|id| !id.is_empty()) {
        // The chat service prepends fresh memory, procedure, and attachment
        // context to the persisted user turn in this in-memory history. A
        // native resume already has the older conversation, but `user_text`
        // alone drops that new context (and is empty for attachment-only
        // messages). Render only the current user entry when it is present.
        if history.last().is_some_and(|entry| {
            matches!(
                entry,
                ChatLlmTranscriptEntry::UserText { .. } | ChatLlmTranscriptEntry::UserTurn { .. }
            )
        }) {
            return compose_turn_text(&history[history.len() - 1..], user_text);
        }
        return user_text.to_string();
    }
    let body = compose_turn_text(history, user_text);
    if matches!(engine_name, "claude_code" | "pi") || system_prompt.is_empty() {
        return body;
    }
    format!("{}\n\n{}", system_prompt.trim_end(), body)
}

fn compose_turn_text(history: &[ChatLlmTranscriptEntry], user_text: &str) -> String {
    use crate::magician_v2::chat::models::TranscriptBlock;

    let mut out = String::new();
    let mut replay_budget = HARNESS_REPLAY_TOTAL_MAX_BYTES;
    let executed_earlier = neutralized_prior_turn_call_ids(history);
    for entry in history {
        match entry {
            ChatLlmTranscriptEntry::UserText { text } => {
                out.push_str("User: ");
                out.push_str(text);
                out.push_str("\n\n");
            },
            ChatLlmTranscriptEntry::UserTurn { content } => {
                let text: String = content
                    .iter()
                    .filter_map(|block| match block {
                        TranscriptBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                let has_non_text = content
                    .iter()
                    .any(|block| !matches!(block, TranscriptBlock::Text { .. }));
                if !text.is_empty() {
                    out.push_str("User: ");
                    out.push_str(&text);
                    if has_non_text {
                        out.push_str(" [attachment omitted from harness prompt]");
                    }
                    out.push_str("\n\n");
                } else if has_non_text {
                    out.push_str("User: [attachment omitted from harness prompt]\n\n");
                }
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text, tool_calls, ..
            } => {
                if let Some(text) = text.as_deref().filter(|value| !value.is_empty()) {
                    out.push_str("Assistant: ");
                    out.push_str(text);
                    out.push_str("\n\n");
                }
                for call in tool_calls {
                    push_tool_call(
                        &mut out,
                        call,
                        executed_earlier.contains(call.id.as_str()),
                        &mut replay_budget,
                    );
                }
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id,
                tool_name,
                content,
            } => {
                if executed_earlier.contains(tool_call_id.as_str()) {
                    continue;
                }
                push_tool_result(&mut out, tool_name.as_deref(), content, &mut replay_budget);
            },
            ChatLlmTranscriptEntry::ToolResultRich {
                tool_call_id,
                tool_name,
                content,
            } => {
                if executed_earlier.contains(tool_call_id.as_str()) {
                    continue;
                }
                // Text blocks are carried; anything else is named, not carried.
                let mut text = String::new();
                for block in content {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    match block {
                        TranscriptBlock::Text { text: block_text } => text.push_str(block_text),
                        _ => text.push_str("[non-text block omitted]"),
                    }
                }
                push_tool_result(&mut out, tool_name.as_deref(), &text, &mut replay_budget);
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id,
                tool_name,
                projection,
            } => {
                if executed_earlier.contains(tool_call_id.as_str()) {
                    continue;
                }
                // The same guarded model value the native mouth replays, under
                // the same admission: the stored projection is never read
                // directly, and one that is not this call's is not replayed.
                let text = (projection.validate_schema_version().is_ok()
                    && projection.identity.tool_call_id == *tool_call_id)
                    .then(|| {
                        crate::magician_v2::tool_result_projection::provider_safe_model_value(
                            projection,
                        )
                    })
                    .and_then(|value| serde_json::to_string(&value).ok());
                match text {
                    Some(text) => {
                        push_tool_result(&mut out, tool_name.as_deref(), &text, &mut replay_budget)
                    },
                    None => push_tool_note(
                        &mut out,
                        tool_name.as_deref(),
                        "result (projection unavailable).",
                        &mut replay_budget,
                    ),
                }
            },
        }
    }
    // The current message is normally the history's last entry already
    // (persisted before the turn runs); it is appended only when the history
    // does not end on the user's side.
    let history_ends_with_user = history.last().is_some_and(|entry| {
        matches!(
            entry,
            ChatLlmTranscriptEntry::UserText { .. } | ChatLlmTranscriptEntry::UserTurn { .. }
        )
    });
    if !user_text.is_empty() && !history_ends_with_user {
        out.push_str("User: ");
        out.push_str(user_text);
        out.push('\n');
    }
    if out.is_empty() {
        user_text.to_string()
    } else {
        out
    }
}

/// Caps on what a cold turn replays of the tools: a cold turn must read as a
/// transcript, not as a dump of every tool's output, and a one-shot engine
/// takes the whole prompt on argv. The per-result cap bounds one carried
/// result; the history cap is charged every replayed call, result and
/// omission line, so a call-heavy history is bounded too.
const HARNESS_REPLAY_RESULT_MAX_BYTES: usize = 4 * 1024;
const HARNESS_REPLAY_TOTAL_MAX_BYTES: usize = 48 * 1024;
/// Cap on the compact JSON arguments named beside a replayed call.
const HARNESS_REPLAY_ARGS_MAX_BYTES: usize = 512;

/// Ids of the task-spawning calls made before the last user message. The
/// native mouth replays those as a note without their arguments and drops
/// their results (`ChatLlmService::neutralized_stale_task_call_ids`): a
/// model that sees the verbatim template re-fires it on an unrelated turn.
fn neutralized_prior_turn_call_ids(history: &[ChatLlmTranscriptEntry]) -> HashSet<&str> {
    let boundary = history
        .iter()
        .rposition(|entry| {
            matches!(
                entry,
                ChatLlmTranscriptEntry::UserText { .. } | ChatLlmTranscriptEntry::UserTurn { .. }
            )
        })
        .unwrap_or(0);
    history[..boundary]
        .iter()
        .filter_map(|entry| match entry {
            ChatLlmTranscriptEntry::AssistantTurn { tool_calls, .. } => Some(tool_calls),
            _ => None,
        })
        .flatten()
        .filter(|call| {
            crate::magician_v2::chat::llm_service::ChatLlmService::is_stale_replay_neutralized_tool(
                &call.name,
            )
        })
        .map(|call| call.id.as_str())
        .collect()
}

/// Write one replayed line and charge it to the history budget.
fn push_replay_line(out: &mut String, line: &str, budget: &mut usize) {
    out.push_str(line);
    *budget = budget.saturating_sub(line.len());
}

fn tool_label(tool_name: Option<&str>) -> String {
    match tool_name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => format!("Tool {name}"),
        None => "Tool".to_string(),
    }
}

/// A one-line note in a result's place, charged like a result.
fn push_tool_note(out: &mut String, tool_name: Option<&str>, note: &str, budget: &mut usize) {
    push_replay_line(
        out,
        &format!("{} {note}\n\n", tool_label(tool_name)),
        budget,
    );
}

/// Name a replayed call ahead of its result so the transcript reads
/// call → result. A prior turn's task spawn is named without its arguments:
/// it is a fact of the conversation, not a template to re-issue.
fn push_tool_call(
    out: &mut String,
    call: &crate::magician_v2::chat::models::StoredToolCall,
    executed_earlier: bool,
    budget: &mut usize,
) {
    let mut line = String::from("Assistant called");
    let name = call.name.trim();
    if !name.is_empty() {
        line.push(' ');
        line.push_str(name);
    }
    if *budget == 0 {
        line.push_str(" (omitted: replay budget spent)\n\n");
    } else if executed_earlier {
        line.push_str(" (arguments omitted: already executed)\n\n");
    } else {
        // Redacted structurally before the head is taken: a named credential
        // field is recognisable only as JSON, and a cut before redaction
        // could keep a secret's head.
        let args =
            serde_json::to_string(&sanitize_json_for_provider(&call.arguments)).unwrap_or_default();
        let head = head_within(&args, HARNESS_REPLAY_ARGS_MAX_BYTES);
        if name.is_empty() {
            line.push(' ');
        }
        line.push('(');
        line.push_str(head);
        if head.len() < args.len() {
            line.push_str(PLANE_TURN_RESULT_CUT_MARK);
        }
        line.push_str(")\n\n");
    }
    push_replay_line(out, &line, budget);
}

/// Append one replayed tool result, bounded per result and by what the
/// history has left. A cut result is marked; once the history budget is
/// spent every further result is named but not carried.
fn push_tool_result(out: &mut String, tool_name: Option<&str>, content: &str, budget: &mut usize) {
    if *budget == 0 {
        push_tool_note(
            out,
            tool_name,
            "result (omitted: replay budget spent).",
            budget,
        );
        return;
    }
    // Redacted before it is cut, as the native mouth redacts each result: the
    // composed prose never takes the sanitizer's structural JSON path, and a
    // secret straddling the cut would otherwise keep its head.
    let content = sanitize_text_for_provider(content);
    let content = content.trim();
    if content.is_empty() {
        push_tool_note(out, tool_name, "returned no text output.", budget);
        return;
    }
    let head = head_within(content, HARNESS_REPLAY_RESULT_MAX_BYTES.min(*budget));
    let mut line = format!("{} result:\n{head}", tool_label(tool_name));
    if head.len() < content.len() {
        line.push_str(PLANE_TURN_RESULT_CUT_MARK);
    }
    line.push_str("\n\n");
    push_replay_line(out, &line, budget);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chat snapshot is process-wide, and libtest runs tests in parallel:
    /// every test that installs one holds this, or another's install lands
    /// between its own install and read.
    static CHAT_SNAPSHOT_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A voice call's chat choice is checked as a typed turn's is: roster
    /// engines only, `magician` always allowed, sane model and profile names.
    #[test]
    fn a_client_chat_choice_is_validated_like_a_typed_turn() {
        let native = validated_client_chat_harness_choice("magician", None, Some("balanced"))
            .expect("the native mouth is always available");
        assert_eq!(native.engine, "magician");
        assert_eq!(native.model, "default");
        assert_eq!(native.profile.as_deref(), Some("balanced"));
        assert!(validated_client_chat_harness_choice("opencode", None, None).is_err());
        assert!(
            validated_client_chat_harness_choice("magician", Some("bad\nmodel"), None).is_err()
        );
        assert!(
            validated_client_chat_harness_choice("magician", Some(&"m".repeat(129)), None).is_err()
        );
        assert!(validated_client_chat_harness_choice("magician", None, Some("bad\u{7}")).is_err());
    }
    use crate::magician_v2::agents::{FeatureMode, InvocationSourceKind};
    use crate::magician_v2::chat::tools_runtime::{
        build_chat_runtime_tools, plane_posture_index, PlanePosture,
    };
    use crate::magician_v2::execution::agentic::policy_snapshot::{
        EffectiveToolGrant, EffectiveToolKind,
    };
    use crate::magician_v2::execution::plane::catalog::{plane_tools_list, test_index_with};
    use std::collections::{BTreeMap, BTreeSet};

    fn chat_invocation() -> AgentInvocationContext {
        AgentInvocationContext {
            principal: "owner".to_string(),
            workspace: "home".to_string(),
            source_agent_id: None,
            target_agent_id: "personal-assistant".to_string(),
            surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::Direct,
            chat_session_id: Some("sess-1".to_string()),
            chat_turn_id: Some("turn-1".to_string()),
        }
    }

    #[test]
    fn pi_profile_model_metadata_change_invalidates_resume() {
        let mut profile = magicllm::config::LlmConfig::default();
        profile.reasoning_effort = Some("high".into());
        let previous = chat_model_fingerprint("pi", "default", Some(&profile));
        profile.reasoning_effort = None;
        assert_ne!(
            previous,
            chat_model_fingerprint("pi", "default", Some(&profile))
        );
        let previous = chat_model_fingerprint("pi", "default", Some(&profile));
        profile.max_tokens = Some(8192);
        assert_ne!(
            previous,
            chat_model_fingerprint("pi", "default", Some(&profile))
        );
    }

    #[test]
    fn magician_skip_drops_a_warm_harness_resume() {
        let session = "abandon-resume-sess";
        forget_chat_harness_conversation(session);
        store_continuation(
            session,
            conversation_generation(session),
            ChatHarnessContinuation {
                native_session_id: Some("native-1".into()),
                grant_token: None,
                engine: Some("claude_code".into()),
                model_fingerprint: None,
                native_home: None,
            },
        );
        abandon_chat_harness_resume(session);
        let after = continuation(session);
        assert!(after.native_session_id.is_none());
        assert!(after.engine.is_none());
        forget_chat_harness_conversation(session);
    }

    /// A continuation with a home under the root, as a harness turn stores.
    fn continuation_with_home(engine: &str) -> ChatHarnessContinuation {
        let home = mint_native_home().expect("mint a native home");
        assert!(home.is_dir());
        assert!(home.starts_with(native_homes_root()));
        ChatHarnessContinuation {
            native_session_id: Some("native-1".to_string()),
            grant_token: None,
            engine: Some(engine.to_string()),
            model_fingerprint: None,
            native_home: Some(home),
        }
    }

    /// Another service process can still be using a home when this process
    /// first opens the shared root. Initialization must preserve it.
    #[test]
    fn preparing_native_homes_root_preserves_existing_sessions() {
        let base = tempfile::tempdir().expect("temporary root");
        let root = base.path().join("plane-homes");
        let active_home = root.join("older-process-session");
        std::fs::create_dir_all(&active_home).expect("existing home");
        let marker = active_home.join("session.json");
        std::fs::write(&marker, b"active").expect("existing session");

        prepare_native_homes_root(&root);

        assert_eq!(
            std::fs::read(&marker).expect("session preserved"),
            b"active"
        );
    }

    #[tokio::test]
    async fn aborting_a_turn_removes_only_its_new_native_home() {
        let active_home = mint_native_home().expect("older active home");
        let new_home = mint_native_home().expect("new turn home");
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let owned = new_home.clone();
        let turn = tokio::spawn(async move {
            let _lease = RemoveNewNativeHomeOnDrop(Some(owned));
            let _ = entered_tx.send(());
            std::future::pending::<()>().await;
        });
        entered_rx.await.expect("turn entered");
        turn.abort();
        let _ = turn.await;

        assert!(!new_home.exists(), "aborted turn removed its new home");
        assert!(active_home.exists(), "older home remains available");
        remove_native_home(Some(active_home));
    }

    /// The Magician mouth taking a turn drops the CLI's persisted session
    /// with the resume id: the home goes too.
    #[test]
    fn abandon_chat_harness_resume_removes_the_native_home() {
        let session = "abandon-home-sess";
        forget_chat_harness_conversation(session);
        let stored = continuation_with_home("codex_app_server");
        let home = stored.native_home.clone().unwrap();
        store_continuation(session, conversation_generation(session), stored);
        assert_eq!(
            continuation(session).native_home.as_deref(),
            Some(home.as_path())
        );

        abandon_chat_harness_resume(session);
        assert!(!home.exists(), "abandon removed the native home");
        assert!(continuation(session).native_home.is_none());
        forget_chat_harness_conversation(session);
    }

    /// Session teardown removes the home with the continuation.
    #[test]
    fn forget_chat_harness_conversation_removes_the_native_home() {
        let session = "forget-home-sess";
        forget_chat_harness_conversation(session);
        let stored = continuation_with_home("codex_app_server");
        let home = stored.native_home.clone().unwrap();
        store_continuation(session, conversation_generation(session), stored);

        forget_chat_harness_conversation(session);
        assert!(!home.exists(), "forget removed the native home");
        assert!(continuation(session).native_home.is_none());
    }

    /// Only a direct child of the root is ever removed: a continuation never
    /// carries another path, a session's temp home is its own, and the root
    /// itself never goes through this path.
    #[test]
    fn a_home_outside_the_root_is_never_removed() {
        let elsewhere = tempfile::tempdir().unwrap();
        let home = elsewhere.path().join("not-a-plane-home");
        std::fs::create_dir_all(&home).unwrap();
        remove_native_home(Some(home.clone()));
        assert!(home.is_dir(), "a path outside the root is left alone");
        remove_native_home(None);

        let root = native_homes_root().to_path_buf();
        remove_native_home(Some(root.clone()));
        assert!(root.is_dir(), "the root itself is never removed");
        remove_native_home(Some(root.parent().unwrap().to_path_buf()));
        assert!(root.is_dir(), "nor anything above it");

        let nested = mint_native_home().unwrap().join("inside");
        std::fs::create_dir_all(&nested).unwrap();
        remove_native_home(Some(nested.clone()));
        assert!(nested.is_dir(), "a path below a home is not a home");
        remove_native_home(nested.parent().map(std::path::Path::to_path_buf));
        assert!(!nested.exists());
    }

    /// A store that loses to the tombstone (the conversation was forgotten
    /// or taken over while its first harness turn was in flight) holds the
    /// only reference to the home that turn minted: the store releases it.
    #[test]
    fn a_late_store_after_a_tombstone_releases_its_home() {
        let session = "late-store-home-sess";
        forget_chat_harness_conversation(session);
        let generation = conversation_generation(session);
        let stored = continuation_with_home("codex_app_server");
        let home = stored.native_home.clone().unwrap();

        forget_chat_harness_conversation(session);
        store_continuation(session, generation, stored);
        assert!(
            continuation(session).native_home.is_none(),
            "the tombstone won"
        );
        assert!(!home.exists(), "and the late store released its home");
    }

    fn settled_with(stop_reason: HarnessStopReason, id: Option<&str>) -> HarnessTurnSettled {
        HarnessTurnSettled {
            assistant_text: String::new(),
            stop_reason,
            usage: None,
            native_session_id: id.map(str::to_string),
        }
    }

    /// The persisted reply of a turn that streamed is the deltas the chat
    /// side collected, whole — the engine's settled text is bounded — plus
    /// what a failing exit appended after them; a turn that streamed
    /// nothing keeps the engine's text.
    #[test]
    fn a_streamed_reply_is_persisted_whole_with_the_engines_appended_text() {
        // Nothing streamed: the engine's text, whatever it is.
        assert_eq!(reply_to_persist("settled".to_string(), ""), "settled");
        assert_eq!(reply_to_persist("settled".to_string(), " \n"), "settled");
        assert_eq!(reply_to_persist(String::new(), ""), "");
        // Streamed and uncut: the engine's text is the deltas whole, and on
        // a failing exit the error after them.
        assert_eq!(reply_to_persist("ok".to_string(), "ok"), "ok");
        assert_eq!(
            reply_to_persist("ok\nerror: rate limited".to_string(), "ok"),
            "ok\nerror: rate limited"
        );
        // Streamed and cut by the engine: the collected deltas, whole.
        let whole = "w".repeat(40 * 1024);
        let cut = format!("{}{PLANE_TURN_RESULT_CUT_MARK}", &whole[..16 * 1024]);
        assert_eq!(reply_to_persist(cut.clone(), &whole), whole);
        // Cut and failed: the error the engine appended after its cut mark
        // survives after the whole deltas.
        assert_eq!(
            reply_to_persist(format!("{cut}\nerror: rate limited"), &whole),
            format!("{whole}\nerror: rate limited")
        );
        // An engine that settled without its streamed text (empty, or
        // something else entirely) still persists what was streamed.
        assert_eq!(reply_to_persist(String::new(), "ok"), "ok");
        assert_eq!(reply_to_persist("other".to_string(), "ok"), "ok");
    }

    /// A refused turn keeps only what the engine reported, so a seeded id
    /// the CLI could not resume is dropped and the next turn runs cold
    /// instead of refusing the same way again; every other stop keeps the
    /// seeded id when the engine reported none.
    #[test]
    fn a_refused_turn_keeps_only_a_reported_native_id() {
        let seeded = Some("seeded-1");
        assert_eq!(
            native_id_to_keep(
                &settled_with(HarnessStopReason::Refused, None),
                seeded,
                true
            ),
            None
        );
        assert_eq!(
            native_id_to_keep(
                &settled_with(HarnessStopReason::Refused, Some("")),
                seeded,
                true
            ),
            None
        );
        assert_eq!(
            native_id_to_keep(
                &settled_with(HarnessStopReason::Refused, Some("confirmed-1")),
                seeded,
                true
            )
            .as_deref(),
            Some("confirmed-1")
        );
        for stop in [
            HarnessStopReason::Settled,
            HarnessStopReason::TurnBudgetSpent,
            HarnessStopReason::Cancelled,
            HarnessStopReason::NeedsApproval,
        ] {
            assert_eq!(
                native_id_to_keep(&settled_with(stop, None), seeded, true).as_deref(),
                Some("seeded-1"),
                "{stop:?} keeps the seeded id"
            );
            assert_eq!(
                native_id_to_keep(&settled_with(stop, Some("new-1")), seeded, true).as_deref(),
                Some("new-1"),
                "{stop:?} prefers the reported id"
            );
        }
        assert_eq!(
            native_id_to_keep(
                &settled_with(HarnessStopReason::Settled, Some("new-1")),
                seeded,
                false
            ),
            None,
            "an engine that cannot resume keeps nothing"
        );
    }

    /// Same engine keeps its home; a switch mints a fresh one and removes
    /// the old (its persisted session belongs to the other CLI); an engine
    /// that cannot resume gets none.
    #[test]
    fn engine_switch_mints_a_fresh_home() {
        let prior = continuation_with_home("codex_app_server");
        let prior_home = prior.native_home.clone().unwrap();

        let same = native_home_for_turn(&prior, "codex_app_server", true);
        assert_eq!(same.as_deref(), Some(prior_home.as_path()));
        assert!(prior_home.is_dir(), "the same engine keeps its home");

        let switched =
            native_home_for_turn(&prior, "claude_code", true).expect("a switch mints a fresh home");
        assert_ne!(switched, prior_home);
        assert!(switched.is_dir());
        assert!(switched.starts_with(native_homes_root()));
        assert!(!prior_home.exists(), "the old engine's home is removed");
        remove_native_home(Some(switched));

        let cold = continuation_with_home("codex_app_server");
        let cold_home = cold.native_home.clone().unwrap();
        assert_eq!(native_home_for_turn(&cold, "codex_app_server", false), None);
        assert!(
            !cold_home.exists(),
            "no home for an engine that cannot resume"
        );

        let none = ChatHarnessContinuation::default();
        let fresh = native_home_for_turn(&none, "codex_app_server", true)
            .expect("a first turn mints a home");
        assert!(fresh.is_dir());
        remove_native_home(Some(fresh));
        assert_eq!(native_home_for_turn(&none, "codex_app_server", false), None);
    }

    /// A home that vanished under a live continuation is re-minted rather
    /// than handed to the CLI as a directory that is not there.
    #[test]
    fn a_vanished_home_is_reminted() {
        let prior = continuation_with_home("codex_app_server");
        let prior_home = prior.native_home.clone().unwrap();
        std::fs::remove_dir_all(&prior_home).unwrap();

        let reminted = native_home_for_turn(&prior, "codex_app_server", true)
            .expect("a vanished home is re-minted");
        assert_ne!(reminted, prior_home);
        assert!(reminted.is_dir());
        remove_native_home(Some(reminted));
    }

    /// Every minted home is private to the service user and unpredictable.
    #[cfg(unix)]
    #[test]
    fn a_minted_home_is_private_and_under_the_root() {
        use std::os::unix::fs::PermissionsExt;
        let home = mint_native_home().expect("mint");
        let mode = std::fs::metadata(&home).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "home mode");
        let root_mode = std::fs::metadata(native_homes_root())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(root_mode, 0o700, "root mode");
        assert_eq!(home.parent(), Some(native_homes_root()));
        remove_native_home(Some(home));
    }

    #[test]
    fn coerce_chat_mouth_accepts_roster_engines() {
        assert_eq!(coerce_chat_mouth_engine("pi"), "pi");
        assert_eq!(coerce_chat_mouth_engine("claude_code"), "claude_code");
        assert_eq!(coerce_chat_mouth_engine("magician"), "magician");
        assert_eq!(coerce_chat_mouth_engine("grok"), "grok");
        assert_eq!(coerce_chat_mouth_engine("codex"), "codex");
        assert_eq!(
            coerce_chat_mouth_engine("codex_app_server"),
            "codex_app_server"
        );
        assert_eq!(coerce_chat_mouth_engine("agy"), "agy");
        assert_eq!(coerce_chat_mouth_engine("not-an-engine"), "magician");
    }

    #[test]
    fn default_snapshot_is_magician() {
        assert_eq!(ChatHarnessSnapshot::default().engine, "magician");
        assert!(matches!(
            resolve_turn_engine(Some(&ChatHarnessSnapshot::default().engine)),
            TurnEngine::MagicianDecision
        ));
    }

    #[test]
    fn unknown_engine_fails_closed_to_magician() {
        assert!(matches!(
            resolve_turn_engine(Some("not-an-engine")),
            TurnEngine::MagicianDecision
        ));
    }

    #[test]
    fn roster_names_select_a_harness() {
        assert!(matches!(
            resolve_turn_engine(Some("claude_code")),
            TurnEngine::Harness("claude_code")
        ));
    }

    #[tokio::test]
    async fn conversation_grant_keeps_chat_surface() {
        let invocation = chat_invocation();
        let mut ctx = AgenticContext::default();
        ctx.invocation_context_override = Some(invocation.clone());
        ctx.principal = Some(invocation.principal.clone());
        ctx.workspace = Some(invocation.workspace.clone());
        ctx.agent_id = Some(invocation.target_agent_id.clone());
        ctx.chat_session_id = invocation.chat_session_id.clone();
        let grant =
            PlaneGrant::for_conversation(ctx, "sess-1".to_string(), CancellationToken::new());
        assert_eq!(grant.surface(), InvocationSurface::Chat);
        let token = plane_grant_registry().mint_chat(grant).await;
        let resolved = plane_grant_registry()
            .resolve(&token)
            .await
            .expect("chat grant must resolve");
        assert_eq!(resolved.surface(), InvocationSurface::Chat);
        assert_eq!(
            resolved.pause_disposition(),
            crate::magician_v2::execution::plane::grant::PlanePauseDisposition::Refuse
        );
        plane_grant_registry().revoke(&token).await;
    }

    fn test_executors() -> crate::magician_v2::execution::agentic::ActionExecutors {
        use crate::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        use crate::magician_v2::test_utils::ConfigurableMockLlm;
        let storage = JsonPromptStorage::with_default_config().expect("prompt storage");
        crate::magician_v2::execution::agentic::ActionExecutors::new(
            Arc::new(ConfigurableMockLlm::with_response("{}")),
            Arc::new(PromptManager::new(Arc::new(storage))),
        )
    }

    /// The ledger the turn keeps before the mint is the one a `tools/call`
    /// on the door's resolved clone writes to, so the outcome carries the
    /// swapped mouth's calls after the grant itself has gone into the
    /// registry. A second drain is empty; a grant without a ledger drains
    /// nothing.
    #[tokio::test]
    async fn the_turn_drains_the_calls_its_minted_grant_dispatched() {
        assert!(drain_turn_ledger(&None).is_empty());

        let invocation = chat_invocation();
        let mut ctx = AgenticContext::default();
        ctx.invocation_context_override = Some(invocation.clone());
        ctx.principal = Some(invocation.principal.clone());
        ctx.workspace = Some(invocation.workspace.clone());
        ctx.agent_id = Some(invocation.target_agent_id.clone());
        ctx.chat_session_id = invocation.chat_session_id.clone();
        let mut grant =
            PlaneGrant::for_conversation(ctx, "sess-ledger".to_string(), CancellationToken::new());
        grant.allowed_tools = vec!["list_tasks".to_string()];
        grant.executors = Some(Arc::new(test_executors()));
        let ledger = grant.turn_ledger.clone();
        assert!(ledger.is_some(), "a conversation grant carries a ledger");

        let token = plane_grant_registry().mint_chat(grant).await;
        let resolved = plane_grant_registry()
            .resolve(&token)
            .await
            .expect("chat grant must resolve");
        let _ = crate::magician_v2::execution::plane::dispatch::plane_tools_call(
            &resolved,
            "list_tasks",
            &serde_json::json!({"status": "open"}),
        )
        .await;
        plane_grant_registry().revoke(&token).await;

        let tool_calls = drain_turn_ledger(&ledger);
        assert_eq!(tool_calls.len(), 1, "{tool_calls:?}");
        assert_eq!(tool_calls[0].tool_name, "list_tasks");
        assert_eq!(
            tool_calls[0].arguments,
            serde_json::json!({"status": "open"})
        );
        assert!(tool_calls[0].call_id.starts_with("pltinv_"));
        assert!(
            drain_turn_ledger(&ledger).is_empty(),
            "the drain empties the ledger"
        );
    }

    /// The ledger persists in batches the store's per-append caps admit:
    /// each batch is one assistant turn naming at most a chunk of calls and
    /// one result per call — balanced on its own — with the ids matching
    /// between the assistant entry and its results and dispatch order kept
    /// across batches. No calls, no batches.
    #[test]
    fn harness_transcript_batches_are_balanced_and_bounded() {
        let calls: Vec<PlaneTurnToolCall> = (0..450)
            .map(|index| PlaneTurnToolCall {
                call_id: format!("pltinv_{index}"),
                tool_name: ["list_tasks", "read_file"][index % 2].to_string(),
                arguments: serde_json::json!({"index": index}),
                content: format!("result {index}"),
                is_error: index % 7 == 0,
            })
            .collect();

        let batches = harness_transcript_batches(&calls, 200);
        let sizes: Vec<usize> = batches.iter().map(Vec::len).collect();
        // One assistant entry naming the chunk, then one result per call.
        assert_eq!(sizes, vec![201, 201, 51]);

        let mut seen = 0usize;
        for batch in &batches {
            let ChatLlmTranscriptEntry::AssistantTurn {
                text,
                tool_calls,
                provider_state,
            } = &batch[0]
            else {
                panic!("a batch opens with the assistant turn naming its calls: {batch:?}");
            };
            assert_eq!(text, &None);
            assert_eq!(provider_state, &None);
            assert_eq!(
                tool_calls.len(),
                batch.len() - 1,
                "one result per named call"
            );
            for (named, result) in tool_calls.iter().zip(&batch[1..]) {
                let ChatLlmTranscriptEntry::ToolResult {
                    tool_call_id,
                    tool_name,
                    content,
                } = result
                else {
                    panic!("every entry after the assistant turn is a result: {result:?}");
                };
                let call = &calls[seen];
                assert_eq!(named.id, call.call_id, "order is kept across batches");
                assert_eq!(named.name, call.tool_name);
                assert_eq!(named.arguments, call.arguments);
                assert_eq!(tool_call_id, &named.id, "the result answers the named call");
                assert_eq!(tool_name.as_deref(), Some(call.tool_name.as_str()));
                assert_eq!(content, &call.content);
                seen += 1;
            }
        }
        assert_eq!(seen, calls.len(), "every call is persisted exactly once");

        assert!(harness_transcript_batches(&[], 200).is_empty());
        assert_eq!(
            harness_transcript_batches(&calls[..3], 0).len(),
            3,
            "a zero chunk still persists, one call per batch"
        );
    }

    #[tokio::test]
    async fn protected_and_meeting_turns_do_not_start_a_harness() {
        let _snapshot = CHAT_SNAPSHOT_GUARD.lock().await;
        install_chat_harness_snapshot(ChatHarnessSnapshot {
            engine: "claude_code".to_string(),
            ..ChatHarnessSnapshot::default()
        });
        let mut invocation = chat_invocation();
        let none = maybe_harness_chat_turn(ChatHarnessTurnRequest {
            invocation: &invocation,
            system_prompt: "sys",
            history: &[],
            user_text: "hi",
            cancel: CancellationToken::new(),
            disclosure_guarded: true,
            token_sink: None,
            approval_rules: &[],
            policy_snapshot: None,
            plane_postures: &HashMap::new(),
            tool_index: None,
            trust_level: None,
            trust_enforcer: None,
            trust_policies_path: None,
            mouth_tool_specs: &[],
            mouth_bridge: None,
            choice: None,
            pi_profile: None,
            pi_images: Vec::new(),
        })
        .await
        .expect("protected skip is not an error");
        assert!(none.is_none());

        invocation.surface = InvocationSurface::Meeting;
        let none = maybe_harness_chat_turn(ChatHarnessTurnRequest {
            invocation: &invocation,
            system_prompt: "sys",
            history: &[],
            user_text: "hi",
            cancel: CancellationToken::new(),
            disclosure_guarded: false,
            token_sink: None,
            approval_rules: &[],
            policy_snapshot: None,
            plane_postures: &HashMap::new(),
            tool_index: None,
            trust_level: None,
            trust_enforcer: None,
            trust_policies_path: None,
            mouth_tool_specs: &[],
            mouth_bridge: None,
            choice: None,
            pi_profile: None,
            pi_images: Vec::new(),
        })
        .await
        .expect("meeting skip is not an error");
        assert!(none.is_none());

        invocation.surface = InvocationSurface::AppCopilot;
        invocation.feature_mode = FeatureMode::AppCopilot;
        let none = maybe_harness_chat_turn(ChatHarnessTurnRequest {
            invocation: &invocation,
            system_prompt: "sys",
            history: &[],
            user_text: "hi",
            cancel: CancellationToken::new(),
            disclosure_guarded: false,
            token_sink: None,
            approval_rules: &[],
            policy_snapshot: None,
            plane_postures: &HashMap::new(),
            tool_index: None,
            trust_level: None,
            trust_enforcer: None,
            trust_policies_path: None,
            mouth_tool_specs: &[],
            mouth_bridge: None,
            choice: None,
            pi_profile: None,
            pi_images: Vec::new(),
        })
        .await
        .expect("app copilot skip is not an error");
        assert!(none.is_none());

        invocation.surface = InvocationSurface::Tutor;
        invocation.feature_mode = FeatureMode::Tutor;
        let none = maybe_harness_chat_turn(ChatHarnessTurnRequest {
            invocation: &invocation,
            system_prompt: "sys",
            history: &[],
            user_text: "hi",
            cancel: CancellationToken::new(),
            disclosure_guarded: false,
            token_sink: None,
            approval_rules: &[],
            policy_snapshot: None,
            plane_postures: &HashMap::new(),
            tool_index: None,
            trust_level: None,
            trust_enforcer: None,
            trust_policies_path: None,
            mouth_tool_specs: &[],
            mouth_bridge: None,
            choice: None,
            pi_profile: None,
            pi_images: Vec::new(),
        })
        .await
        .expect("tutor skip is not an error");
        assert!(none.is_none());

        invocation.surface = InvocationSurface::PublicEnvoy;
        invocation.feature_mode = FeatureMode::None;
        let none = maybe_harness_chat_turn(ChatHarnessTurnRequest {
            invocation: &invocation,
            system_prompt: "sys",
            history: &[],
            user_text: "hi",
            cancel: CancellationToken::new(),
            disclosure_guarded: false,
            token_sink: None,
            approval_rules: &[],
            policy_snapshot: None,
            plane_postures: &HashMap::new(),
            tool_index: None,
            trust_level: None,
            trust_enforcer: None,
            trust_policies_path: None,
            mouth_tool_specs: &[],
            mouth_bridge: None,
            choice: None,
            pi_profile: None,
            pi_images: Vec::new(),
        })
        .await
        .expect("public envoy skip is not an error");
        assert!(none.is_none());
        install_chat_harness_snapshot(ChatHarnessSnapshot::default());
    }

    /// A plain chat turn with no resolved policy fails closed: no snapshot
    /// means no hands, and no hands means the harness is never started. The
    /// no-hands skip abandons the warm resume (one generation bump, as the
    /// no-engine skip also does), so the bump pins that an abandoning skip
    /// returned, not a disclosure, surface, or not-installed one.
    #[tokio::test]
    async fn a_chat_turn_without_a_policy_snapshot_does_not_start_a_harness() {
        let _snapshot = CHAT_SNAPSHOT_GUARD.lock().await;
        install_chat_harness_snapshot(ChatHarnessSnapshot {
            engine: "claude_code".to_string(),
            ..ChatHarnessSnapshot::default()
        });
        let session = "no-snapshot-sess";
        let mut invocation = chat_invocation();
        invocation.chat_session_id = Some(session.to_string());
        let before = conversation_generation(session);
        let none = maybe_harness_chat_turn(ChatHarnessTurnRequest {
            invocation: &invocation,
            system_prompt: "sys",
            history: &[],
            user_text: "hi",
            cancel: CancellationToken::new(),
            disclosure_guarded: false,
            token_sink: None,
            approval_rules: &[],
            policy_snapshot: None,
            plane_postures: &HashMap::new(),
            tool_index: None,
            trust_level: None,
            trust_enforcer: None,
            trust_policies_path: None,
            mouth_tool_specs: &[],
            mouth_bridge: None,
            choice: None,
            pi_profile: None,
            pi_images: Vec::new(),
        })
        .await
        .expect("no-snapshot skip is not an error");
        assert!(none.is_none());
        assert_eq!(
            conversation_generation(session),
            before + 1,
            "the no-hands skip must abandon the warm resume"
        );
        install_chat_harness_snapshot(ChatHarnessSnapshot::default());
    }

    /// The chat mouth is the parent of the turn's background operations;
    /// the native mouth — configured, or an unknown name coerced to it at
    /// install — names none.
    #[test]
    fn a_chat_turn_on_a_harness_names_it_as_parent() {
        let _snapshot = CHAT_SNAPSHOT_GUARD.blocking_lock();
        install_chat_harness_snapshot(ChatHarnessSnapshot {
            engine: "grok".to_string(),
            ..ChatHarnessSnapshot::default()
        });
        assert_eq!(chat_turn_parent_engine().as_deref(), Some("grok"));

        install_chat_harness_snapshot(ChatHarnessSnapshot {
            engine: "magician".to_string(),
            ..ChatHarnessSnapshot::default()
        });
        assert_eq!(
            chat_turn_parent_engine(),
            None,
            "the native loop is no parent"
        );

        install_chat_harness_snapshot(ChatHarnessSnapshot {
            engine: "not-a-roster-engine".to_string(),
            ..ChatHarnessSnapshot::default()
        });
        assert_eq!(
            chat_turn_parent_engine(),
            None,
            "an unknown mouth is the native one"
        );
        install_chat_harness_snapshot(ChatHarnessSnapshot::default());
    }

    #[test]
    fn continuation_is_per_conversation() {
        store_continuation(
            "sess-a",
            0,
            ChatHarnessContinuation {
                native_session_id: Some("native-a".to_string()),
                grant_token: None,
                engine: Some("claude_code".to_string()),
                model_fingerprint: None,
                native_home: None,
            },
        );
        store_continuation(
            "sess-b",
            0,
            ChatHarnessContinuation {
                native_session_id: Some("native-b".to_string()),
                grant_token: None,
                engine: Some("claude_code".to_string()),
                model_fingerprint: None,
                native_home: None,
            },
        );
        assert_eq!(
            continuation("sess-a").native_session_id.as_deref(),
            Some("native-a")
        );
        assert_eq!(
            continuation("sess-b").native_session_id.as_deref(),
            Some("native-b")
        );
        forget_chat_harness_conversation("sess-a");
        assert!(continuation("sess-a").native_session_id.is_none());
        assert_eq!(
            continuation("sess-b").native_session_id.as_deref(),
            Some("native-b")
        );
        forget_chat_harness_conversation("sess-b");
    }

    #[test]
    fn forget_wins_over_a_late_continuation_store() {
        store_continuation(
            "sess-forget",
            0,
            ChatHarnessContinuation {
                native_session_id: Some("native-old".to_string()),
                grant_token: None,
                engine: Some("claude_code".to_string()),
                model_fingerprint: None,
                native_home: None,
            },
        );
        forget_chat_harness_conversation("sess-forget");
        store_continuation(
            "sess-forget",
            0,
            ChatHarnessContinuation {
                native_session_id: Some("native-stale".to_string()),
                grant_token: None,
                engine: Some("claude_code".to_string()),
                model_fingerprint: None,
                native_home: None,
            },
        );
        assert!(continuation("sess-forget").native_session_id.is_none());
    }

    #[test]
    fn terminal_usage_chat_preserves_cache_reads_without_inventing_model_or_cost() {
        let (input, output, cached) = harness_usage_to_chat_tokens(Some(&HarnessUsage {
            input_tokens: 12,
            cached_input_tokens: 9,
            output_tokens: 4,
            ..Default::default()
        }));
        assert_eq!((input, output, cached), (12, 4, 9));
        assert_eq!(harness_usage_to_chat_tokens(None), (0, 0, 0));
    }

    fn grant(name: &str, kind: EffectiveToolKind) -> (String, EffectiveToolGrant) {
        (
            name.to_string(),
            EffectiveToolGrant {
                name: name.to_string(),
                kind,
                provider_visible: true,
            },
        )
    }

    /// A snapshot with exactly the tools named; everything else empty.
    fn policy_snapshot(
        direct: &[&str],
        deferred: &[&str],
        runtime: &[&str],
        denied: &[&str],
    ) -> crate::magician_v2::execution::agentic::EffectiveToolPolicySnapshot {
        let direct_tools: BTreeMap<_, _> = direct
            .iter()
            .map(|name| grant(name, EffectiveToolKind::Direct))
            .collect();
        let deferred_tools: BTreeMap<_, _> = deferred
            .iter()
            .map(|name| grant(name, EffectiveToolKind::Deferred))
            .collect();
        let runtime_tools: BTreeMap<_, _> = runtime
            .iter()
            .map(|name| grant(name, EffectiveToolKind::Runtime))
            .collect();
        // Direct + Runtime are provider-callable now; Deferred only after load.
        let dispatch_tool_names: BTreeSet<String> = direct
            .iter()
            .chain(runtime.iter())
            .map(|s| s.to_string())
            .collect();
        crate::magician_v2::execution::agentic::EffectiveToolPolicySnapshot {
            snapshot_id: "snap".to_string(),
            agent_id: "personal-assistant".to_string(),
            definition_version: 1,
            definition_digest: "digest".to_string(),
            invocation: chat_invocation(),
            trust_level: "local".to_string(),
            direct_tools,
            deferred_tools,
            runtime_tools,
            implicit_tools: BTreeSet::from(["task_state".to_string()]),
            structural_tools: BTreeMap::new(),
            delegation_targets: BTreeMap::new(),
            handover_targets: BTreeMap::new(),
            denied_tool_names: denied.iter().map(|s| s.to_string()).collect(),
            denied_tool_params: HashMap::new(),
            approval_rules: Vec::new(),
            provider_specs: Vec::new(),
            dispatch_tool_names,
            delegate_owned_tool_names: BTreeSet::new(),
            engagement_authority: None,
        }
    }

    fn real_postures() -> HashMap<String, PlanePosture> {
        plane_posture_index(&build_chat_runtime_tools())
    }

    #[test]
    fn a_runtime_tool_with_a_counterpart_crosses_as_the_counterpart() {
        let snapshot = policy_snapshot(&[], &[], &["get_task_details_for_chat"], &[]);
        let allowed = plane_hands_allowlist(&snapshot, &real_postures());
        assert_eq!(allowed, vec!["get_task_details".to_string()]);
    }

    /// A Bridged runtime tool contributes nothing to the plane-executed
    /// allowlist: it rides the bridged set (`plane_bridged_specs`) instead.
    #[test]
    fn a_bridged_runtime_tool_does_not_cross_the_plane_executed_allowlist() {
        let snapshot = policy_snapshot(
            &[],
            &[],
            &[
                "subscribe_to_task_for_chat",
                "list_tools_for_chat",
                "start_tutor_run",
            ],
            &[],
        );
        assert!(plane_hands_allowlist(&snapshot, &real_postures()).is_empty());
    }

    #[test]
    fn a_runtime_control_unknown_to_the_index_does_not_cross() {
        // Server-owned controls added via extend_snapshot_with_runtime_tool
        // are Runtime-kind but are not ChatRuntimeTools; they stay native.
        let snapshot = policy_snapshot(&[], &[], &["request_thinking_mode"], &[]);
        assert!(plane_hands_allowlist(&snapshot, &real_postures()).is_empty());
    }

    #[test]
    fn deferred_tools_cross_so_the_harness_can_write() {
        let snapshot = policy_snapshot(&["http"], &["update_task", "web_search"], &[], &[]);
        let allowed = plane_hands_allowlist(&snapshot, &real_postures());
        assert!(allowed.contains(&"http".to_string()));
        assert!(allowed.contains(&"update_task".to_string()));
        assert!(allowed.contains(&"web_search".to_string()));
    }

    #[test]
    fn the_floor_still_applies_after_the_snapshot() {
        let snapshot = policy_snapshot(
            &[
                "read_file",         // stays
                "shell",             // NEVER_ON_THE_PLANE
                "read_result",       // PLANE_CONTROL_VERBS
                "create_task",       // NOT_A_PLANE_EXECUTED_HAND
                "run_task",          // NOT_A_PLANE_EXECUTED_HAND
                "app_action_invoke", // governed app compiled tool
                "http",              // denied below
            ],
            &[],
            &[],
            &["http"],
        );
        let allowed = plane_hands_allowlist(&snapshot, &real_postures());
        assert_eq!(allowed, vec!["read_file".to_string()]);
    }

    #[test]
    fn a_denied_runtime_tool_does_not_cross_as_its_counterpart() {
        let snapshot = policy_snapshot(
            &[],
            &[],
            &["get_task_details_for_chat"],
            &["get_task_details_for_chat"],
        );
        assert!(plane_hands_allowlist(&snapshot, &real_postures()).is_empty());
    }

    #[test]
    fn an_empty_snapshot_yields_no_hands() {
        let snapshot = policy_snapshot(&[], &[], &[], &[]);
        assert!(plane_hands_allowlist(&snapshot, &real_postures()).is_empty());
        assert!(plane_hands_allowlist(&snapshot, &HashMap::new()).is_empty());
    }

    #[test]
    fn hands_are_ordered_compiled_then_counterparts() {
        let snapshot = policy_snapshot(&["read_file"], &[], &["get_task_details_for_chat"], &[]);
        let allowed = plane_hands_allowlist(&snapshot, &real_postures());
        assert_eq!(
            allowed,
            vec!["read_file".to_string(), "get_task_details".to_string()]
        );
    }

    #[test]
    fn a_counterpart_that_is_also_direct_appears_once() {
        let snapshot = policy_snapshot(
            &["read_file", "get_task_details"],
            &[],
            &["get_task_details_for_chat"],
            &[],
        );
        let allowed = plane_hands_allowlist(&snapshot, &real_postures());
        assert_eq!(
            allowed,
            vec!["get_task_details".to_string(), "read_file".to_string()]
        );
    }

    #[test]
    fn a_denied_counterpart_is_dropped_even_when_its_runtime_tool_is_permitted() {
        let snapshot = policy_snapshot(
            &[],
            &[],
            &["get_task_details_for_chat"],
            &["get_task_details"],
        );
        assert!(plane_hands_allowlist(&snapshot, &real_postures()).is_empty());
    }

    /// A swapped mouth loads Deferred hands through its grant-scoped
    /// `tool_search`, and the door checks the allowlist before intercepting
    /// it — so the floor must let `tool_search` cross.
    #[test]
    fn tool_search_survives_the_floor_so_a_swapped_mouth_can_load_deferred_hands() {
        let snapshot = policy_snapshot(&["tool_search", "http"], &["update_task"], &[], &[]);
        let allowed = plane_hands_allowlist(&snapshot, &real_postures());
        assert!(allowed.contains(&"tool_search".to_string()), "{allowed:?}");
    }

    #[test]
    fn the_floor_predicate_matches_every_list_the_builder_applies() {
        for name in [
            "shell",
            "read_result",
            "create_task",
            "run_task",
            "switch_personality",
            "app_action_invoke",
        ] {
            assert!(
                chat_harness_floor_rejects(name),
                "{name} should be rejected by the floor"
            );
        }
        for name in [
            "read_file",
            "http",
            "get_task_details",
            "get_agent_details",
            "update_task",
        ] {
            assert!(
                !chat_harness_floor_rejects(name),
                "{name} should pass the floor"
            );
        }
    }

    fn mouth_spec(name: &str) -> LLMToolSpec {
        LLMToolSpec {
            name: name.to_string(),
            description: format!("{name} on the native mouth"),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"title": {"type": "string"}},
            }),
        }
    }

    /// The bridged set is the native mouth's specs minus what the plane
    /// executes itself, minus the counterparts, under the bridge floor: the
    /// plane-executed floor's own names cross (`create_task`,
    /// `switch_personality`), and so does `read_result`; the nesting floor
    /// (`shell`), the door's own verbs (`tool_search`, `session_ledger`),
    /// the loop's intercepted verbs (an adaptive profile's thinking-mode
    /// escalation, which the native loop answers before dispatch and the
    /// dispatcher would take for a pack), the loop's other control verbs
    /// (`yield`), governed app tools, and anything in the plane allowlist
    /// do not. A `Counterpart`'s native name (`get_task_details_for_chat`,
    /// in the mouth's specs beside its plane twin) does not either, or the
    /// mouth would get the same tool twice; nor does a `MouthOnly` name.
    /// Every name the plane-executed floor rejects for being a mouth tool
    /// is bridgeable, so nothing the native mouth advertises falls between
    /// the two sets.
    #[test]
    fn bridged_specs_exclude_plane_executed_and_nesting_floor_names() {
        let specs: Vec<LLMToolSpec> = [
            "create_task",
            "shell",
            "read_result",
            "yield",
            "read_file",
            "switch_personality",
            "app_action_invoke",
            "get_task_details",
            "get_task_details_for_chat",
            "describe_agents_for_chat",
            "subscribe_to_task_for_chat",
            "create_chat_thread",
            "tool_search",
            "session_ledger",
            crate::magician_v2::chat::service::REQUEST_THINKING_MODE_TOOL_NAME,
        ]
        .iter()
        .map(|name| mouth_spec(name))
        .collect();
        let plane_allowlist = vec!["read_file".to_string(), "get_task_details".to_string()];
        let bridged = plane_bridged_specs(&specs, &plane_allowlist, &real_postures());
        let names: Vec<&str> = bridged.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            vec![
                "create_chat_thread",
                "create_task",
                "read_result",
                "subscribe_to_task_for_chat",
                "switch_personality",
            ]
        );
        assert_eq!(
            bridged["create_task"],
            mouth_spec("create_task"),
            "the spec crosses whole"
        );

        let mut withheld = real_postures();
        withheld.insert("create_chat_thread".to_string(), PlanePosture::MouthOnly);
        assert!(
            !plane_bridged_specs(&specs, &plane_allowlist, &withheld)
                .contains_key("create_chat_thread"),
            "a MouthOnly name is withheld from the bridged set"
        );

        for name in NOT_A_PLANE_EXECUTED_HAND {
            assert!(chat_harness_bridgeable(name), "{name} bridgeable");
            assert!(
                chat_harness_floor_rejects(name),
                "{name} not plane-executed"
            );
        }
        for name in [
            "shell",
            "yield",
            "need_user_input",
            "spawn_sub_goal",
            "app_action_invoke",
            "tool_search",
            "session_ledger",
            crate::magician_v2::chat::service::REQUEST_THINKING_MODE_TOOL_NAME,
        ] {
            assert!(!chat_harness_bridgeable(name), "{name} must not bridge");
        }
        for name in LOOP_INTERCEPTED_VERBS {
            assert!(
                !chat_harness_bridgeable(name),
                "{name} is the loop's, not the dispatcher's"
            );
        }
        assert!(plane_bridged_specs(&[], &plane_allowlist, &real_postures()).is_empty());
    }

    /// A bridge that records every `(call_id, name, arguments)` it is
    /// handed and answers with a fixed object, as the chat service's bridge
    /// answers with the native dispatcher's result.
    #[allow(clippy::type_complexity)]
    fn recording_bridge(
        answer: serde_json::Value,
    ) -> (
        ChatMouthBridge,
        Arc<std::sync::Mutex<Vec<(String, String, serde_json::Value)>>>,
    ) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let bridge: ChatMouthBridge = Arc::new(
            move |call_id: String,
                  name: String,
                  arguments: serde_json::Value,
                  _cancel: CancellationToken| {
                let record = Arc::clone(&record);
                let answer = answer.clone();
                Box::pin(async move {
                    record
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push((call_id, name, arguments));
                    answer
                })
            },
        );
        (bridge, seen)
    }

    /// The bridge the chat service builds crosses the request onto the
    /// grant the turn mints: a bridged name the swapped mouth calls reaches
    /// the bridge with its name and arguments verbatim under the call id
    /// the turn ledger records, and what the bridge answers is what the
    /// mouth reads back — the plane adds no wrapper. The plane-executed
    /// hand stays permitted beside the bridged one.
    #[tokio::test]
    async fn a_bridge_minted_through_the_request_sees_the_call_and_answers_verbatim() {
        let specs: Vec<LLMToolSpec> = ["create_chat_thread", "list_tasks", "shell"]
            .iter()
            .map(|name| mouth_spec(name))
            .collect();
        let answer = serde_json::json!({"status": "ok", "thread_id": "thr-9"});
        let (bridge, seen) = recording_bridge(answer.clone());

        let mut grant = conversation_grant_with_index(&["list_tasks"], &[]);
        grant.executors = Some(Arc::new(test_executors()));
        let grant =
            bridge_mouth_tools(grant, &specs, Some(bridge), &real_postures(), None, "codex");
        let bridged: Vec<&str> = grant.bridged_tools.keys().map(String::as_str).collect();
        assert_eq!(
            bridged,
            vec!["create_chat_thread"],
            "plane-executed and floor names stay off"
        );
        assert!(grant.permits("create_chat_thread"));
        assert!(
            grant.permits("list_tasks"),
            "the plane-executed hand is still the grant's"
        );
        assert!(advertised(&grant).contains("create_chat_thread"));
        let ledger = grant.turn_ledger.clone();

        let token = plane_grant_registry().mint_chat(grant).await;
        let resolved = plane_grant_registry()
            .resolve(&token)
            .await
            .expect("chat grant must resolve");
        let arguments = serde_json::json!({"title": "Plans", "nested": {"n": 1}});
        let result = crate::magician_v2::execution::plane::dispatch::plane_tools_call(
            &resolved,
            "create_chat_thread",
            &arguments,
        )
        .await;
        plane_grant_registry().revoke(&token).await;

        let calls = seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(calls.len(), 1, "{calls:?}");
        let (call_id, name, seen_arguments) = &calls[0];
        assert_eq!(name, "create_chat_thread");
        assert_eq!(
            seen_arguments, &arguments,
            "the bridge saw the arguments verbatim"
        );
        assert_eq!(result["isError"], serde_json::json!(false), "{result}");
        let text = result["content"][0]["text"].as_str().expect("text");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(text).expect("compact JSON"),
            answer,
            "the mouth reads back exactly what the bridge answered"
        );
        let recorded = drain_turn_ledger(&ledger);
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0].tool_name, "create_chat_thread");
        assert_eq!(recorded[0].arguments, arguments);
        assert_eq!(
            &recorded[0].call_id, call_id,
            "the bridge ran under the id the ledger records"
        );
    }

    /// The specs alone offer nothing: without a bridge there would be no
    /// way to run them, so the grant is returned untouched.
    #[test]
    fn specs_without_a_bridge_offer_nothing() {
        let specs = vec![mouth_spec("create_chat_thread")];
        let grant = bridge_mouth_tools(
            conversation_grant_with_index(&["list_tasks"], &[]),
            &specs,
            None,
            &real_postures(),
            None,
            "codex",
        );
        assert!(grant.bridged_tools.is_empty());
        assert!(grant.mouth_bridge.is_none());
        assert!(!grant.permits("create_chat_thread"));
        assert_eq!(grant.allowed_tools, vec!["list_tasks".to_string()]);
    }

    /// A compiled name the policy grants but the plane never executes itself
    /// (`create_task`) is not in the provider's hot spec list — a Deferred
    /// tool reaches the native mouth only after a `tool_search` load. The
    /// mint bridges it from the index anyway, so the swapped mouth is not
    /// the one mouth that cannot create a task.
    #[test]
    fn a_floor_rejected_deferred_hand_is_bridged_from_the_index() {
        let (bridge, _seen) = recording_bridge(serde_json::json!({"status": "ok"}));
        let policy = policy_snapshot(&["list_tasks"], &["create_task", "web_search"], &[], &[]);
        let grant = bridge_mouth_tools(
            conversation_grant_with_index(&["list_tasks"], &["list_tasks", "create_task"]),
            &[],
            Some(bridge),
            &real_postures(),
            Some(&policy),
            "codex",
        );
        let bridged: Vec<&str> = grant.bridged_tools.keys().map(String::as_str).collect();
        assert_eq!(
            bridged,
            vec!["create_task"],
            "only the floor-rejected granted name"
        );
        assert!(grant.permits("create_task"));
        assert!(
            !grant.bridged_tools.contains_key("web_search"),
            "a plane-executable deferred hand is not bridged"
        );

        let denied = policy_snapshot(&[], &["create_task"], &[], &["create_task"]);
        let (bridge, _seen) = recording_bridge(serde_json::json!({"status": "ok"}));
        let grant = bridge_mouth_tools(
            conversation_grant_with_index(&["list_tasks"], &["create_task"]),
            &[],
            Some(bridge),
            &real_postures(),
            Some(&denied),
            "codex",
        );
        assert!(
            grant.bridged_tools.is_empty(),
            "a denied name is never bridged"
        );
    }

    /// A settled turn whose ledger holds one record per `(name, result)`,
    /// the record's `is_error` read from the result as the door reads it.
    fn outcome_with_results(calls: &[(&str, serde_json::Value)]) -> ChatHarnessTurnOutcome {
        ChatHarnessTurnOutcome {
            decision_model_calls: Vec::new(),
            settled: HarnessTurnSettled {
                assistant_text: String::new(),
                stop_reason: HarnessStopReason::Settled,
                native_session_id: None,
                usage: None,
            },
            tool_calls: calls
                .iter()
                .map(|(name, result)| PlaneTurnToolCall {
                    call_id: format!("pltinv_{name}"),
                    tool_name: name.to_string(),
                    arguments: serde_json::Value::Null,
                    content: result.to_string(),
                    is_error: result["status"] == "error",
                })
                .collect(),
        }
    }

    /// Every named call answered with a plain ok object.
    fn outcome_with_calls(names: &[&str]) -> ChatHarnessTurnOutcome {
        let calls: Vec<(&str, serde_json::Value)> = names
            .iter()
            .map(|name| (*name, serde_json::json!({"status": "ok"})))
            .collect();
        outcome_with_results(&calls)
    }

    /// A turn ends the warm resume only when a persona or skill switch
    /// landed, read from the recorded result: the handlers' own success
    /// objects count; the switch tool's list mode, a re-activation of the
    /// active skill, a deactivation with nothing active, an executor-path
    /// envelope whose run did not succeed, an errored call, and a record
    /// that is not JSON all keep the resume — a false positive removes the
    /// native home, which nothing restores. Reads and task creation keep it.
    #[test]
    fn a_turn_that_bridged_a_persona_change_is_flagged() {
        assert!(!outcome_with_calls(&[]).changed_persona());
        assert!(!outcome_with_calls(&["list_tasks", "create_task"]).changed_persona());
        for name in PERSONA_CHANGING_TOOLS {
            assert!(
                outcome_with_calls(&["list_tasks", name]).changed_persona(),
                "{name} changes the persona"
            );
            assert!(chat_harness_bridgeable(name), "{name} crosses the bridge");
        }

        let landed = [
            (
                "switch_personality",
                serde_json::json!({"status": "ok", "mode": "focused", "source": "skill"}),
            ),
            (
                "activate_skill",
                serde_json::json!({"status": "ok", "skill": "playbook", "outcome": "first"}),
            ),
            (
                "activate_skill",
                serde_json::json!({"status": "ok", "skill": "playbook", "outcome": "replaced"}),
            ),
            (
                "deactivate_skill",
                serde_json::json!({"status": "ok", "outcome": "deactivated"}),
            ),
            (
                "switch_personality",
                serde_json::json!({
                    "status": "ok",
                    "success": true,
                    "terminal_decision": "success",
                }),
            ),
        ];
        for (name, result) in landed {
            assert!(
                outcome_with_results(&[(name, result.clone())]).changed_persona(),
                "{name} landed: {result}"
            );
        }

        let unchanged = [
            (
                "switch_personality",
                serde_json::json!({"status": "ok", "action": "list", "available_modes": []}),
            ),
            (
                "activate_skill",
                serde_json::json!({"status": "ok", "skill": "playbook", "outcome": "idempotent"}),
            ),
            (
                "deactivate_skill",
                serde_json::json!({"status": "ok", "outcome": "noop"}),
            ),
            (
                "switch_personality",
                serde_json::json!({
                    "status": "ok",
                    "success": false,
                    "terminal_decision": "failed",
                }),
            ),
            (
                "switch_personality",
                serde_json::json!({"status": "error", "reason": "Personality preset not found."}),
            ),
            (
                "activate_skill",
                serde_json::json!({
                    "status": "error",
                    "reason": "activate_skill requires a non-empty `name`",
                }),
            ),
        ];
        for (name, result) in unchanged {
            assert!(
                !outcome_with_results(&[(name, result.clone())]).changed_persona(),
                "{name} changed nothing: {result}"
            );
        }

        let mut cut = outcome_with_calls(&["switch_personality"]);
        cut.tool_calls[0].content = "{\"status\":\"ok\",\"mode\": …[cut]".to_string();
        assert!(
            !cut.changed_persona(),
            "a record cut by the ledger bound is not a switch"
        );
        let mut errored = outcome_with_calls(&["switch_personality"]);
        errored.tool_calls[0].is_error = true;
        assert!(
            !errored.changed_persona(),
            "an errored call is not a switch"
        );
    }

    fn conversation_grant_with_index(allowed: &[&str], index: &[&str]) -> PlaneGrant {
        let invocation = chat_invocation();
        let mut ctx = AgenticContext::default();
        ctx.invocation_context_override = Some(invocation.clone());
        ctx.principal = Some(invocation.principal.clone());
        ctx.workspace = Some(invocation.workspace.clone());
        ctx.agent_id = Some(invocation.target_agent_id.clone());
        ctx.chat_session_id = invocation.chat_session_id.clone();
        let mut grant =
            PlaneGrant::for_conversation(ctx, "sess-preload".to_string(), CancellationToken::new())
                .with_tool_index(Arc::new(test_index_with(index)));
        grant.allowed_tools = allowed.iter().map(|s| s.to_string()).collect();
        grant
    }

    fn advertised(grant: &PlaneGrant) -> HashSet<String> {
        plane_tools_list(grant)
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_string))
            .collect()
    }

    fn names(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// Hot names and names the index does not know are never preloaded; the
    /// hot list is the profile's, so a leaf hot on spawned-bare is a
    /// preload candidate on terminal.
    #[test]
    fn deferred_hands_to_preload_are_the_indexed_non_hot_allowlist() {
        let index = test_index_with(&["read_file", "duckdb__query", "web_search"]);
        let allowed: Vec<String> = [
            "read_file",
            "get_task_details",
            "duckdb__query",
            "web_search",
            "not_in_index",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            deferred_hands_to_preload(&allowed, &index, PlaneCatalogProfile::SpawnedBare),
            names(&["duckdb__query", "web_search"])
        );
        assert_eq!(
            deferred_hands_to_preload(&allowed, &index, PlaneCatalogProfile::Terminal),
            names(&["read_file", "duckdb__query", "web_search"])
        );
    }

    /// The mint-block wiring: an engine that never re-lists tools gets its
    /// Deferred hands loaded before the first `tools/list`, and that list
    /// advertises them.
    #[test]
    fn a_mouth_that_cannot_relist_gets_its_deferred_hands_at_mint() {
        let mut grant = conversation_grant_with_index(
            &[
                "read_file",
                "get_task_details",
                "duckdb__query",
                "web_search",
                "tool_search",
            ],
            &["read_file", "duckdb__query", "web_search"],
        );
        assert!(
            !advertised(&grant).contains("web_search"),
            "hot before mint"
        );

        assert_eq!(preload_deferred_hands(&mut grant, false), 2);
        assert!(grant.preloaded_deferred);
        assert_eq!(
            grant.loaded_tool_names(),
            names(&["duckdb__query", "web_search"])
        );
        let after = advertised(&grant);
        for name in [
            "duckdb__query",
            "web_search",
            "read_file",
            "get_task_details",
        ] {
            assert!(
                after.contains(name),
                "{name} missing from the first tools/list: {after:?}"
            );
        }
    }

    #[test]
    fn a_mouth_that_relists_loads_nothing_at_mint() {
        let mut grant = conversation_grant_with_index(
            &["read_file", "duckdb__query", "web_search"],
            &["read_file", "duckdb__query", "web_search"],
        );
        assert_eq!(preload_deferred_hands(&mut grant, true), 0);
        assert!(!grant.preloaded_deferred);
        assert!(grant.loaded_tool_names().is_empty());
        let after = advertised(&grant);
        assert!(!after.contains("duckdb__query"), "{after:?}");
        assert!(!after.contains("web_search"), "{after:?}");
    }

    /// Nothing to preload leaves the grant on the replace path: the flag
    /// must not flip on an empty load.
    #[test]
    fn an_allowlist_with_no_deferred_hands_does_not_mark_the_grant_preloaded() {
        let mut grant =
            conversation_grant_with_index(&["read_file", "get_task_details"], &["read_file"]);
        assert_eq!(preload_deferred_hands(&mut grant, false), 0);
        assert!(!grant.preloaded_deferred);
        assert!(grant.loaded_tool_names().is_empty());
    }

    #[test]
    fn compose_includes_user_and_assistant_turns() {
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "hello".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("hi".to_string()),
                tool_calls: Vec::new(),
                provider_state: None,
            },
        ];
        let text = compose_turn_text(&history, "again");
        assert!(text.contains("hello"));
        assert!(text.contains("hi"));
        assert!(text.contains("again"));
    }

    fn replayed_result(index: usize, content: String) -> ChatLlmTranscriptEntry {
        ChatLlmTranscriptEntry::ToolResult {
            tool_call_id: format!("call-{index}"),
            tool_name: Some("read_file".to_string()),
            content,
        }
    }

    fn replayed_call(id: &str, name: &str, arguments: serde_json::Value) -> ChatLlmTranscriptEntry {
        ChatLlmTranscriptEntry::AssistantTurn {
            text: None,
            tool_calls: vec![crate::magician_v2::chat::models::StoredToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments,
            }],
            provider_state: None,
        }
    }

    /// A projection with no app checkpoint: the guarded reader hands back the
    /// model value itself.
    fn replayed_projection(
        call_id: &str,
        value: serde_json::Value,
    ) -> crate::magician_v2::tool_result_projection::ProjectedToolResultV1 {
        use crate::magician_v2::tool_result_projection::{
            DisplayResultProjection, ModelResultProjection, ProjectedToolResultV1,
            ProjectionMetrics, ProjectionStrategy, RawResultDescriptor, ResultRetentionClass,
            ScopedResultRef, ToolOutcome, ToolResultIdentity,
            TOOL_RESULT_PROJECTION_SCHEMA_VERSION,
        };
        let raw = RawResultDescriptor {
            content_ref: ScopedResultRef {
                result_ref: format!("result-{call_id}"),
                cursor: None,
            },
            content_hash: format!("sha256-{call_id}"),
            media_type: "application/json".to_string(),
            size_bytes: 1,
            retention_class: ResultRetentionClass::ChatSession,
        };
        ProjectedToolResultV1 {
            schema_version: TOOL_RESULT_PROJECTION_SCHEMA_VERSION,
            identity: ToolResultIdentity {
                tool_name: "memory_search".to_string(),
                tool_call_id: call_id.to_string(),
                execution_id: None,
                task_id: None,
                scope_digest: "scope-digest".to_string(),
                authority_revision: "authority-1".to_string(),
            },
            outcome: ToolOutcome::succeeded(),
            model: ModelResultProjection {
                value,
                strategy: ProjectionStrategy::Complete,
                included_records: 1,
                omitted_records: 0,
                omitted_fields: 0,
                continuation: None,
            },
            app_result_checkpoint: None,
            spoken: None,
            display: DisplayResultProjection::referenced(&raw),
            raw,
            metrics: ProjectionMetrics {
                raw_bytes: 1,
                model_bytes: 1,
                estimated_model_tokens: 1,
                spoken_characters: 0,
                included_records: 1,
                omitted_records: 0,
                omitted_fields: 0,
                maximum_input_depth: 1,
                contract_fallback: false,
            },
        }
    }

    const RESULT_HEADER: &str = "Tool read_file result:\n";
    const BUDGET_SPENT: &str = "(omitted: replay budget spent)";

    /// One result is cut at the per-result cap and says so; a long history
    /// carries results verbatim until the total cap, then names the rest.
    #[test]
    fn cold_replay_carries_tool_results_within_budget() {
        let big = "x".repeat(10 * 1024);
        let history = vec![replayed_result(0, big.clone())];
        let text = compose_turn_text(&history, "again");
        let body = text.split(RESULT_HEADER).nth(1).expect("result header");
        let carried = body.split(PLANE_TURN_RESULT_CUT_MARK).next().expect("head");
        assert_eq!(carried.len(), HARNESS_REPLAY_RESULT_MAX_BYTES);
        assert!(body.starts_with(&big[..HARNESS_REPLAY_RESULT_MAX_BYTES]));
        assert!(
            text.contains(PLANE_TURN_RESULT_CUT_MARK),
            "a cut result must be marked: {text}"
        );
        assert!(!text.contains(&big), "the whole result must not be carried");
        assert!(text.ends_with("User: again\n"));

        // Whole lines (header, content, blank line) that exactly fill the
        // history budget, so the boundary falls between two results.
        let per_result = "y".repeat(HARNESS_REPLAY_RESULT_MAX_BYTES - RESULT_HEADER.len() - 2);
        let line_len = RESULT_HEADER.len() + per_result.len() + 2;
        assert_eq!(
            HARNESS_REPLAY_TOTAL_MAX_BYTES % line_len,
            0,
            "fixture: whole lines"
        );
        let fits = HARNESS_REPLAY_TOTAL_MAX_BYTES / line_len;
        let history: Vec<_> = (0..fits + 8)
            .map(|index| replayed_result(index, per_result.clone()))
            .collect();
        let text = compose_turn_text(&history, "again");
        assert_eq!(
            text.matches(RESULT_HEADER).count(),
            fits,
            "results carried until the total cap: {text}"
        );
        assert_eq!(
            text.matches(&format!("Tool read_file result {BUDGET_SPENT}."))
                .count(),
            8
        );
        assert_eq!(
            text.matches(&per_result).count(),
            fits,
            "every carried result is verbatim"
        );
        assert!(!text.contains(PLANE_TURN_RESULT_CUT_MARK));
        let last_carried = text.rfind(RESULT_HEADER).expect("last carried");
        let first_omitted = text.find(BUDGET_SPENT).expect("first omitted");
        assert!(last_carried < first_omitted);

        // A total cap that lands mid-result cuts that result at what is
        // left, then omits the rest.
        let left = 10;
        let mut history: Vec<_> = (0..fits - 1)
            .map(|index| replayed_result(index, per_result.clone()))
            .collect();
        history.push(replayed_result(
            fits - 1,
            "z".repeat(per_result.len() - left),
        ));
        history.push(replayed_result(fits, "w".repeat(100)));
        history.push(replayed_result(fits + 1, "v".repeat(100)));
        let text = compose_turn_text(&history, "again");
        assert!(
            text.contains(&format!(
                "{}{}",
                "w".repeat(left),
                PLANE_TURN_RESULT_CUT_MARK
            )),
            "{text}"
        );
        assert!(!text.contains(&"w".repeat(left + 1)));
        assert!(!text.contains(&"v".repeat(100)));
        assert_eq!(text.matches(BUDGET_SPENT).count(), 1);

        // The cut lands on a character boundary, never inside one.
        let wide_char = '\u{20ac}';
        let wide = wide_char
            .to_string()
            .repeat(HARNESS_REPLAY_RESULT_MAX_BYTES);
        let text = compose_turn_text(&[replayed_result(0, wide)], "again");
        let body = text.split(RESULT_HEADER).nth(1).expect("result header");
        let carried = body.split(PLANE_TURN_RESULT_CUT_MARK).next().expect("head");
        assert!(carried.len() <= HARNESS_REPLAY_RESULT_MAX_BYTES);
        assert!(carried.len() + wide_char.len_utf8() > HARNESS_REPLAY_RESULT_MAX_BYTES);
        assert!(carried.chars().all(|c| c == wide_char));
    }

    #[test]
    fn cold_replay_names_the_calls_before_their_results() {
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "look it up".to_string(),
            },
            replayed_call(
                "call-1",
                "web_search",
                serde_json::json!({ "query": "tides" }),
            ),
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "call-1".to_string(),
                tool_name: Some("web_search".to_string()),
                content: "high at noon".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Noon.".to_string()),
                tool_calls: Vec::new(),
                provider_state: None,
            },
        ];
        let text = compose_turn_text(&history, "again");
        let call = text
            .find("Assistant called web_search({\"query\":\"tides\"})")
            .expect("call named");
        let result = text
            .find("Tool web_search result:\nhigh at noon")
            .expect("result carried");
        let reply = text.find("Assistant: Noon.").expect("reply");
        assert!(call < result && result < reply, "{text}");
        assert!(!text.contains("omitted"), "{text}");

        // Oversized arguments are cut at their head and marked.
        let history = vec![replayed_call(
            "call-2",
            "write_file",
            serde_json::json!({ "body": "b".repeat(4 * 1024) }),
        )];
        let text = compose_turn_text(&history, "again");
        let args = text
            .split("Assistant called write_file(")
            .nth(1)
            .and_then(|rest| rest.split(")\n").next())
            .expect("call named");
        assert!(args.ends_with(PLANE_TURN_RESULT_CUT_MARK), "{args}");
        assert_eq!(
            args.len() - PLANE_TURN_RESULT_CUT_MARK.len(),
            HARNESS_REPLAY_ARGS_MAX_BYTES
        );
    }

    /// The secrets here are ones the prose pass over the composed text
    /// cannot see (a JSON-form field, a key whose end marker lies past the
    /// cut), so only per-content redaction before the cut removes them.
    #[test]
    fn cold_replay_redacts_before_it_cuts() {
        let password = "hunter2-plain-value";
        let key_body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQ".repeat(4);
        let key = format!("-----BEGIN PRIVATE KEY-----\n{key_body}\n-----END PRIVATE KEY-----");
        let straddling = format!("{}{key}", "x".repeat(HARNESS_REPLAY_RESULT_MAX_BYTES - 100));
        let api_key = "topsecret-plain-value";
        let history = vec![
            replayed_call(
                "call-1",
                "web_search",
                serde_json::json!({ "api_key": api_key, "body": "b".repeat(1024) }),
            ),
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "call-1".to_string(),
                tool_name: Some("web_search".to_string()),
                content: format!("{{\"password\":\"{password}\",\"user\":\"gd\"}}"),
            },
            replayed_result(2, straddling),
        ];
        let text = compose_turn_text(&history, "again");
        assert!(!text.contains(password), "{text}");
        assert!(text.contains("\"password\":\"[REDACTED]\""), "{text}");
        assert!(!text.contains(api_key), "{text}");
        assert!(
            text.contains("Assistant called web_search({\"api_key\":\"[REDACTED]\",\"body\":\"bbb"),
            "{text}"
        );
        assert!(!text.contains(&key_body[..40]), "{text}");
        assert!(!text.contains("BEGIN PRIVATE KEY"), "{text}");
        assert!(text.contains("[REDACTED_PRIVATE_KEY]"), "{text}");
    }

    /// The whole cold input, through `turn_input_text`: the outer pass over
    /// the prose and the per-content passes overlap rather than compete.
    #[test]
    fn cold_replay_through_turn_input_keeps_the_redacted_lines() {
        let password = "hunter2-plain-value";
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "log me in".to_string(),
            },
            replayed_call(
                "call-1",
                "http_request",
                serde_json::json!({ "api_key": "topsecret-plain-value", "url": "https://x" }),
            ),
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "call-1".to_string(),
                tool_name: Some("http_request".to_string()),
                content: format!("{{\"password\":\"{password}\"}}"),
            },
        ];
        let text = turn_input_text(&history, "again", "Be terse.", "codex", None);
        assert!(text.starts_with("Be terse."), "{text}");
        assert!(!text.contains(password), "{text}");
        assert!(!text.contains("topsecret-plain-value"), "{text}");
        assert!(
            text.contains(
                "Assistant called http_request({\"api_key\":\"[REDACTED]\",\"url\":\"https://x\"})"
            ),
            "{text}"
        );
        assert!(
            text.contains("Tool http_request result:\n{\"password\":\"[REDACTED]\"}"),
            "{text}"
        );
        assert!(text.ends_with("User: again\n"), "{text}");
    }

    /// A task spawn before the last user message is a fact, not a template:
    /// it is named without arguments and its result is dropped, as the
    /// native mouth does. The in-flight turn's spawn keeps both.
    #[test]
    fn cold_replay_neutralizes_prior_turn_task_spawns() {
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "ship it".to_string(),
            },
            replayed_call(
                "call-1",
                "create_task",
                serde_json::json!({ "title": "Ship v2" }),
            ),
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "call-1".to_string(),
                tool_name: Some("create_task".to_string()),
                content: "task-111 created".to_string(),
            },
            replayed_call(
                "call-2",
                "web_search",
                serde_json::json!({ "query": "tides" }),
            ),
            ChatLlmTranscriptEntry::ToolResultRich {
                tool_call_id: "call-2".to_string(),
                tool_name: Some("web_search".to_string()),
                content: vec![crate::magician_v2::chat::models::TranscriptBlock::Text {
                    text: "high at noon".to_string(),
                }],
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Done.".to_string()),
                tool_calls: Vec::new(),
                provider_state: None,
            },
            ChatLlmTranscriptEntry::UserText {
                text: "and a second one".to_string(),
            },
            replayed_call(
                "call-3",
                "create_task",
                serde_json::json!({ "title": "Second" }),
            ),
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "call-3".to_string(),
                tool_name: Some("create_task".to_string()),
                content: "task-222 created".to_string(),
            },
        ];
        let text = compose_turn_text(&history, "");
        assert!(
            text.contains("Assistant called create_task (arguments omitted: already executed)\n\n"),
            "{text}"
        );
        assert!(!text.contains("Ship v2"), "{text}");
        assert!(!text.contains("task-111"), "{text}");
        assert!(
            text.contains("Assistant called web_search({\"query\":\"tides\"})"),
            "a prior turn's ordinary call keeps its arguments: {text}"
        );
        assert!(
            text.contains("Tool web_search result:\nhigh at noon"),
            "{text}"
        );
        assert!(
            text.contains("Assistant called create_task({\"title\":\"Second\"})"),
            "the in-flight turn's spawn is intact: {text}"
        );
        assert!(
            text.ends_with("Tool create_task result:\ntask-222 created\n\n"),
            "{text}"
        );
    }

    /// Call lines spend the same budget as results, so a call-heavy history
    /// stays bounded; a spent budget names each further call and result.
    #[test]
    fn cold_replay_charges_call_lines_to_the_budget() {
        let calls = 200;
        let mut history: Vec<_> = (0..calls)
            .map(|index| {
                replayed_call(
                    &format!("call-{index}"),
                    "web_search",
                    serde_json::json!({ "query": "q".repeat(HARNESS_REPLAY_ARGS_MAX_BYTES) }),
                )
            })
            .collect();
        history.push(replayed_result(calls, "late".to_string()));
        let text = compose_turn_text(&history, "again");
        let carried = text.matches("Assistant called web_search(").count();
        let omitted = text
            .matches(&format!("Assistant called web_search {BUDGET_SPENT}\n\n"))
            .count();
        assert!(omitted > 0, "{carried} carried, none omitted");
        assert_eq!(carried + omitted, calls);
        let first_omitted = text.find(BUDGET_SPENT).expect("an omitted call");
        let last_carried = text
            .rfind("Assistant called web_search(")
            .expect("a carried call");
        assert!(last_carried < first_omitted);
        let line_len = text.find(")\n\n").expect("first call line") + ")\n\n".len();
        // The budget is spent by whole lines: the omission line begins at
        // most one call line past the bound.
        let first_omitted_line = text[..first_omitted]
            .rfind("Assistant called")
            .expect("the omitted call's line");
        assert!(
            first_omitted_line <= HARNESS_REPLAY_TOTAL_MAX_BYTES + line_len,
            "the carried calls overshoot the budget by at most one line: {first_omitted_line}"
        );
        assert!(
            text.contains(&format!("Tool read_file result {BUDGET_SPENT}.\n\n")),
            "{text}"
        );
        assert!(!text.contains("late"));
    }

    #[test]
    fn cold_replay_writes_no_text_output_for_empty_results() {
        let history = vec![
            replayed_result(0, String::new()),
            replayed_result(1, "  \n\t".to_string()),
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "call-2".to_string(),
                tool_name: None,
                content: String::new(),
            },
        ];
        let text = compose_turn_text(&history, "again");
        assert_eq!(
            text.matches("Tool read_file returned no text output.\n\n")
                .count(),
            2,
            "{text}"
        );
        assert!(text.contains("Tool returned no text output.\n\n"), "{text}");
        assert!(!text.contains(RESULT_HEADER), "{text}");
    }

    #[test]
    fn cold_replay_renders_rich_text_blocks_and_names_the_rest() {
        use crate::magician_v2::chat::models::{PromptImageRef, TranscriptBlock};
        let history = vec![ChatLlmTranscriptEntry::ToolResultRich {
            tool_call_id: "call-1".to_string(),
            tool_name: Some("screenshot".to_string()),
            content: vec![
                TranscriptBlock::Text {
                    text: "captured the window".to_string(),
                },
                TranscriptBlock::ImageFile {
                    image: PromptImageRef {
                        stored_name: "shot.png".to_string(),
                        mime_type: "image/png".to_string(),
                        label: None,
                    },
                },
                TranscriptBlock::Text {
                    text: "done".to_string(),
                },
            ],
        }];
        let text = compose_turn_text(&history, "again");
        assert!(
            text.contains(
                "Tool screenshot result:\ncaptured the window\n[non-text block omitted]\ndone\n\n"
            ),
            "{text}"
        );
        assert!(!text.contains("shot.png"));
    }

    #[test]
    fn warm_resume_keeps_current_turn_context_and_attachment_description() {
        use crate::magician_v2::chat::models::{PromptImageRef, TranscriptBlock};

        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "old request".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("old reply".to_string()),
                tool_calls: Vec::new(),
                provider_state: None,
            },
            ChatLlmTranscriptEntry::UserTurn {
                content: vec![
                    TranscriptBlock::Text {
                        text: "Fresh memory and procedure context\n\n".to_string(),
                    },
                    TranscriptBlock::Text {
                        text: "File: notes.pdf (application/pdf)".to_string(),
                    },
                    TranscriptBlock::ImageFile {
                        image: PromptImageRef {
                            stored_name: "image.png".to_string(),
                            mime_type: "image/png".to_string(),
                            label: None,
                        },
                    },
                ],
            },
        ];
        let text = turn_input_text_unsanitized(&history, "", "system", "pi", Some("native-id"));
        assert!(
            text.contains("Fresh memory and procedure context"),
            "{text}"
        );
        assert!(text.contains("notes.pdf"), "{text}");
        assert!(text.contains("attachment omitted"), "{text}");
        assert!(!text.contains("old request"), "{text}");
        assert!(!text.contains("old reply"), "{text}");
    }

    /// The projected value is replayed only when the projection is this
    /// call's; a foreign or unversioned one is named as unavailable.
    #[test]
    fn cold_replay_carries_the_projected_model_value() {
        let history = vec![
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-1".to_string(),
                tool_name: Some("memory_search".to_string()),
                projection: replayed_projection(
                    "call-1",
                    serde_json::json!({ "hits": [{ "title": "tide table" }] }),
                ),
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-2".to_string(),
                tool_name: Some("memory_search".to_string()),
                projection: replayed_projection("call-9", serde_json::json!({ "hits": [] })),
            },
        ];
        let text = compose_turn_text(&history, "again");
        let expected = "Tool memory_search result:\n{\"hits\":[{\"title\":\"tide table\"}]}\n\n";
        assert!(text.contains(expected), "{text}");
        assert!(
            text.contains("Tool memory_search result (projection unavailable).\n\n"),
            "{text}"
        );
        assert!(!text.contains("\"hits\":[]"), "{text}");
    }

    #[test]
    fn chat_turn_warm_resume_sends_only_the_new_user_text() {
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "hello".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("hi".to_string()),
                tool_calls: Vec::new(),
                provider_state: None,
            },
        ];
        assert_eq!(
            turn_input_text(
                &history,
                "again",
                "persona",
                "claude_code",
                Some("native-sess")
            ),
            "again"
        );
        let cold = turn_input_text(&history, "again", "persona", "claude_code", None);
        assert!(cold.contains("hello"));
        assert!(cold.contains("again"));
        assert!(!cold.contains("persona"));
        let empty_id = turn_input_text(&history, "again", "", "codex", Some(""));
        assert!(empty_id.contains("hello"));
        let oneshot = turn_input_text(&history, "again", "Be terse.", "codex", None);
        assert!(oneshot.starts_with("Be terse."));
        assert!(oneshot.contains("hello"));
        let switched = turn_input_text(&history, "again", "", "codex", None);
        assert!(switched.contains("hello"));
    }

    // ---- §15 item 5: harness-bound context is redacted like the provider path

    #[test]
    fn harness_turn_input_is_redacted_like_the_provider_path() {
        let key_body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQ";
        let key = format!("-----BEGIN PRIVATE KEY-----\n{key_body}\n-----END PRIVATE KEY-----");
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: format!("here is my key {key}"),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("noted".to_string()),
                tool_calls: Vec::new(),
                provider_state: None,
            },
        ];
        let cold = turn_input_text(&history, "again", "", "codex", None);
        assert!(
            cold.contains("[REDACTED_PRIVATE_KEY]"),
            "cold turn not redacted: {cold}"
        );
        assert!(!cold.contains(key_body));
        assert!(
            cold.contains("again"),
            "the user text must survive redaction"
        );

        let warm = turn_input_text(&[], &key, "", "codex", Some("native-1"));
        assert!(
            !warm.contains(key_body),
            "warm resume text not redacted: {warm}"
        );
    }
}
