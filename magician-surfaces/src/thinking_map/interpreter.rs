//! Live Thinking Map — utterance interpreter (Phase 3, first slice).
//!
//! "The model proposes bounded operations; deterministic code owns state."
//!
//! This module turns a *finalized* speech utterance + a bounded slice of the
//! current board into a validated, model-authored [`MapOperationEnvelope`]. The
//! LLM never sees or emits raw internal [`MapOperation`] JSON — instead it emits
//! a constrained [`ProposedOperation`] whose schema ONLY offers the model-safe
//! subset, so the model literally cannot express an owner-only op. Rust then
//! deterministically translates proposals into canonical operations, minting
//! ids, stamping `assertion_origin = model_inferred` + `epistemic_state =
//! provisional`, and attaching a [`SourceRef`] back to the utterance. The result
//! is wrapped in an `actor = Model` envelope which the reducer authority-validates
//! as a final gate (defense in depth).
//!
//! Dormant + self-contained: no runtime wiring, no route/service registration,
//! no live-LLM dependency (the [`InterpreterLlm`] trait is mocked in tests; the
//! production `OperationLlmRouter` adapter is a LATER phase and is NOT built
//! here). Deterministic where it counts: [`translate`] takes an injected
//! `applied_at` and mints ids via the existing helpers — the only non-determinism
//! is id minting (uuid) and the LLM itself.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::context::{build_context, ContextBudget};
use super::models::{
    new_clarification_id, new_edge_id, new_node_id, AssertionOrigin, Clarification,
    ClarificationState, EdgeKind, EpistemicState, NodeKind, SourceRef, ThinkingEdge, ThinkingMap,
    ThinkingNode,
};
use super::operations::{new_envelope_id, MapOperation, MapOperationEnvelope, OperationActor};

/// The LLM boundary. A production impl (LATER phase) wraps the operation LLM
/// router; tests supply a fake returning canned JSON.
#[async_trait]
pub trait InterpreterLlm: Send + Sync {
    /// Return the raw model completion (expected to be JSON per the prompt).
    async fn complete(&self, system: &str, user: &str) -> anyhow::Result<String>;
}

// ── Progress narration ────────────────────────────────────────────────────────

/// Where an interpretation is in its pipeline, for the humans watching it.
///
/// Interpretation takes seconds — the LLM call dominates — and every client
/// showed a spinner and one unchanging line for the whole of it. iOS wrote the
/// vocabulary for something better and never wired it, because nothing emitted
/// stages on any surface. These are that vocabulary, **bound to the numbered
/// steps of [`interpret`]** rather than invented: every stage here corresponds
/// to a line of that function, which is what keeps a label from narrating work
/// that does not happen (the fate of iOS's `openingThread` and `grounding`,
/// deliberately absent here — no step backs them).
///
/// `Idle` is terminal and is emitted by the *caller* once the whole
/// interpretation settles (applied, zero-move, or failed) — the interpreter
/// itself cannot know when the produced envelope lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterpretStage {
    /// Reading the bounded slice of the board. Carries the live-node count so
    /// a client can say "reading 34 thoughts" — the number is what makes the
    /// wait feel like something is happening.
    Preparing,
    /// Handing the facilitator the graph slice and the intent directive.
    LoadingContext,
    /// The LLM call — the stage that takes the seconds.
    Facilitating,
    /// Reading the model's answer back (one repair attempt, no second call).
    Parsing,
    /// Translating proposals into canonical, provenance-stamped operations.
    Shaping,
    /// Settled: applied, zero-move, or failed. Ready for another direction.
    Idle,
}

impl InterpretStage {
    /// The wire name, matching the serde `snake_case` rename. Kept as an
    /// explicit method because transport events carry the stage as a plain
    /// string, and deriving the name at the emit site from serde would tie
    /// event construction to serialization.
    pub fn wire_name(&self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::LoadingContext => "loading_context",
            Self::Facilitating => "facilitating",
            Self::Parsing => "parsing",
            Self::Shaping => "shaping",
            Self::Idle => "idle",
        }
    }
}

/// Where interpretation narrates its stages.
///
/// The API layer implements this over the transport broadcaster; the ambient
/// coordinator and tests that don't care pass [`NoProgress`]. Best-effort by
/// contract, exactly as `ThinkingMapUpdated` is: an interpretation must never
/// fail because nobody was listening, so implementations must not error and
/// must not block.
pub trait InterpretProgressSink: Send + Sync {
    fn stage(&self, stage: InterpretStage, node_count: Option<usize>);
}

/// The sink for callers with nobody watching.
pub struct NoProgress;

impl InterpretProgressSink for NoProgress {
    fn stage(&self, _stage: InterpretStage, _node_count: Option<usize>) {}
}

/// The user's steering intent for one interpretation pass. E0 sends this
/// alongside the utterance: `continue_thinking` keeps the map incremental
/// (the smallest useful set of moves), while `break_open` asks the model to
/// explode the active thought into a few complementary directions.
///
/// This only *guides the model* via the system prompt — the interpreter does
/// NOT itself cap the operation count. The reducer/translation already bound
/// safety, so an over-eager model cannot exceed the safe subset regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InterpretIntent {
    /// Keep the map incremental — the smallest useful set of moves (0–2).
    #[default]
    ContinueThinking,
    /// Break the active thought open into 2–4 complementary directions.
    BreakOpen,
}

/// A finalized speech utterance to interpret against the map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    pub utterance_id: String,
    pub text: String,
    pub thread_id: Option<String>,
    /// RFC3339, carried into the node/edge `SourceRef`.
    pub timestamp: Option<String>,
}

// ── Model-facing proposal schema (the safe subset ONLY) ──────────────────────

/// A single operation the LLM is allowed to propose. Internally tagged on `op`.
///
/// This schema is intentionally a strict SUBSET of the internal
/// [`MapOperation`] surface: it offers only model-safe ops (content, epistemic
/// state, connection, clarification). There is deliberately NO variant for
/// owner-only operations (move/position-lock/shared-view/promotion/speaker
/// rename/restructure confirm-reject), so a compromised or adversarial model
/// cannot even express one — the unsafe ops are unreachable by construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ProposedOperation {
    /// Add a new node. `temp_id` is a model-chosen handle (e.g. "n1") used to
    /// reference this new node from a `connect` in the SAME batch.
    AddNode {
        temp_id: String,
        kind: NodeKind,
        label: String,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        confidence: Option<f32>,
    },
    /// Patch an existing node's label/detail/confidence.
    UpdateNode {
        node_id: String,
        #[serde(default)]
        label: Option<String>,
        /// `Some(None)` clears detail; `Some(Some(s))` sets it; absent leaves it.
        #[serde(default)]
        detail: Option<Option<String>>,
        #[serde(default)]
        confidence: Option<f32>,
    },
    SetNodeKind {
        node_id: String,
        kind: NodeKind,
    },
    /// Set epistemic state. `confirmed` is REJECTED at translation (a model may
    /// never confirm) — the op is dropped with a skipped-reason.
    SetEpistemicState {
        node_id: String,
        state: EpistemicState,
    },
    TombstoneNode {
        node_id: String,
    },
    /// Connect two nodes. `from`/`to` are EITHER an existing `node_id` OR a
    /// `temp_id` from an `add_node` in the SAME batch.
    Connect {
        from: String,
        to: String,
        kind: EdgeKind,
    },
    CreateClarification {
        node_id: String,
        question: String,
    },
}

/// The whole LLM response — a batch of proposed operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterpretationResponse {
    #[serde(default)]
    pub operations: Vec<ProposedOperation>,
}

// ── Translation: proposals → canonical MapOperations ─────────────────────────

/// Translate a batch of model proposals into canonical [`MapOperation`]s.
///
/// Deterministic modulo id minting (uuid). Rules:
/// - `add_node` mints a real node id, stamps `model_inferred` + `provisional`,
///   clamps confidence to `0.0..=1.0` (default 0.5), and attaches a `SourceRef`
///   citing the utterance. The `temp_id → real id` mapping is recorded so a
///   later `connect` in the same batch resolves.
/// - `connect` resolves `from`/`to` through the temp map (an id that is not a
///   temp id is treated as an existing node id), mints an edge id, stamps
///   `model_inferred`. Self-loops (after resolution) are dropped defensively —
///   the reducer would reject the whole envelope otherwise, and a self-loop is
///   never a meaningful model inference.
/// - `set_epistemic_state` with `confirmed` is DROPPED (fail-safe: a model may
///   not confirm). All other states translate 1:1.
/// - `update_node` / `set_node_kind` / `tombstone_node` / `create_clarification`
///   translate 1:1.
/// - Proposal order is preserved.
pub fn translate(
    proposals: &[ProposedOperation],
    _map: &ThinkingMap,
    utterance: &Utterance,
    applied_at: &str,
) -> Vec<MapOperation> {
    let mut ops: Vec<MapOperation> = Vec::with_capacity(proposals.len());
    // temp_id -> minted real node id, for intra-batch connect resolution.
    let mut temp_ids: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    let source_ref = || SourceRef {
        utterance_id: Some(utterance.utterance_id.clone()),
        thread_id: utterance.thread_id.clone(),
        quote: Some(utterance.text.clone()),
        timestamp: utterance.timestamp.clone(),
    };

    for proposal in proposals {
        match proposal {
            ProposedOperation::AddNode {
                temp_id,
                kind,
                label,
                detail,
                confidence,
            } => {
                let node_id = new_node_id();
                temp_ids.insert(temp_id.clone(), node_id.clone());
                let confidence = confidence.unwrap_or(0.5).clamp(0.0, 1.0);
                let node = ThinkingNode {
                    node_id,
                    kind: *kind,
                    label: label.clone(),
                    detail_markdown: detail.clone(),
                    epistemic_state: EpistemicState::Provisional,
                    assertion_origin: AssertionOrigin::ModelInferred,
                    confidence,
                    speaker: None,
                    source_refs: vec![source_ref()],
                    parent_id: None,
                    position: None,
                    position_locked: false,
                    promoted_refs: vec![],
                    tombstoned: false,
                    created_at: applied_at.to_string(),
                    updated_at: applied_at.to_string(),
                };
                ops.push(MapOperation::AddNode { node });
            },
            ProposedOperation::UpdateNode {
                node_id,
                label,
                detail,
                confidence,
            } => {
                ops.push(MapOperation::UpdateNode {
                    node_id: node_id.clone(),
                    label: label.clone(),
                    detail_markdown: detail.clone(),
                    confidence: *confidence,
                });
            },
            ProposedOperation::SetNodeKind { node_id, kind } => {
                ops.push(MapOperation::SetNodeKind {
                    node_id: node_id.clone(),
                    kind: *kind,
                });
            },
            ProposedOperation::SetEpistemicState { node_id, state } => {
                // Fail-safe: a model may never confirm. Drop the op entirely.
                if matches!(state, EpistemicState::Confirmed) {
                    continue;
                }
                ops.push(MapOperation::SetEpistemicState {
                    node_id: node_id.clone(),
                    state: *state,
                });
            },
            ProposedOperation::TombstoneNode { node_id } => {
                ops.push(MapOperation::TombstoneNode {
                    node_id: node_id.clone(),
                });
            },
            ProposedOperation::Connect { from, to, kind } => {
                // Resolve temp ids (fall through to a literal existing node id).
                let from_id = temp_ids.get(from).cloned().unwrap_or_else(|| from.clone());
                let to_id = temp_ids.get(to).cloned().unwrap_or_else(|| to.clone());
                // Defensive: drop a self-loop the reducer would reject anyway.
                if from_id == to_id {
                    continue;
                }
                let edge = ThinkingEdge {
                    edge_id: new_edge_id(),
                    from_node: from_id,
                    to_node: to_id,
                    kind: *kind,
                    assertion_origin: AssertionOrigin::ModelInferred,
                    tombstoned: false,
                    created_at: applied_at.to_string(),
                    updated_at: applied_at.to_string(),
                };
                ops.push(MapOperation::Connect { edge });
            },
            ProposedOperation::CreateClarification { node_id, question } => {
                let clarification = Clarification {
                    clarification_id: new_clarification_id(),
                    node_id: node_id.clone(),
                    question: question.clone(),
                    state: ClarificationState::Open,
                    answer: None,
                    created_at: applied_at.to_string(),
                    resolved_at: None,
                };
                ops.push(MapOperation::CreateClarification { clarification });
            },
        }
    }

    ops
}

// ── Prompt ───────────────────────────────────────────────────────────────────

/// Inline versioned system prompt BASE (shared across intents). Migrating to a
/// managed prompt file under `data/magician_v2/prompts/` is a follow-on; an
/// inline `const` is intentional for this dormant slice (no config/file
/// touches).
///
/// The intent-specific directive (continue-thinking vs break-open) is appended
/// by [`system_prompt`]; the base holds only the shared, intent-independent
/// rules (safe-subset, no confirm, JSON-only).
const INTERPRETER_SYSTEM_PROMPT_BASE: &str = r#"You are the interpreter for a Live Thinking Map. As the user speaks, you
maintain a typed, revisable map of their thinking: ideas, facts, questions,
decisions, options, risks, actions, metrics, assumptions, evidence, and groups
are first-class nodes; edges relate them.

Given the CURRENT MAP CONTEXT (a bounded slice — never the whole board) and the
latest finalized UTTERANCE, decide the operations that best capture what the
utterance changed. If the utterance adds nothing structural (chit-chat, filler,
an aside), return an empty operations list — it is correct and expected to do
nothing and let the user keep talking.

You MUST respond with ONLY a JSON object of this exact shape and nothing else:
{"operations": [ ... ]}

Each operation is one of (the `op` field selects the variant):
- {"op":"add_node","temp_id":"n1","kind":"idea|fact|question|decision|option|risk|action|metric|assumption|evidence|group","label":"short label","detail":"optional markdown","confidence":0.0-1.0}
- {"op":"update_node","node_id":"<existing id>","label":"...","detail":"...","confidence":0.0-1.0}
- {"op":"set_node_kind","node_id":"<existing id>","kind":"..."}
- {"op":"set_epistemic_state","node_id":"<existing id>","state":"provisional|asserted|contradicted|rejected|resolved|superseded"}
- {"op":"tombstone_node","node_id":"<existing id>"}
- {"op":"connect","from":"<node_id or temp_id>","to":"<node_id or temp_id>","kind":"related_to|supports|contradicts|answers|depends_on|leads_to|alternative_to|measures|grouped_under"}
- {"op":"create_clarification","node_id":"<existing id>","question":"..."}

Rules:
- `temp_id` is your own handle for a node you create in THIS batch; reference it
  from a `connect` in the same batch. To connect to an EXISTING node, use its
  real `node_id` from the context.
- You may NOT confirm a node (there is no "confirmed" state available to you) and
  you may NOT move nodes, change the shared view, or promote nodes — those are
  the owner's alone and are not expressible here.
- Prefer few, high-signal operations. Keep labels short. Do not invent node ids
  that are not in the context.
- Respond with the JSON object only. No prose, no code fences."#;

/// The `continue_thinking` directive: keep the map incremental. Appended to the
/// base for [`InterpretIntent::ContinueThinking`].
const CONTINUE_THINKING_DIRECTIVE: &str = r#"INTENT — CONTINUE THINKING:
Return the SMALLEST useful set — zero to two operations. Zero is correct when
the utterance adds nothing structural."#;

/// The `break_open` directive: explode the active thought into complementary
/// directions. Appended to the base for [`InterpretIntent::BreakOpen`].
const BREAK_OPEN_DIRECTIVE: &str = r#"INTENT — BREAK OPEN:
The user asked to break open the current thought. Return two to four
COMPLEMENTARY directions from different angles — prefer one per node kind (e.g.
an option, a risk, a question, an assumption) — each connected to the active
thought. Do not merely restate it."#;

/// Build the full system prompt for an intent: the shared base plus the
/// intent-specific directive. The interpreter does NOT itself cap the operation
/// count (translation/the reducer already bound safety); the directive only
/// guides the model.
fn system_prompt(intent: InterpretIntent) -> String {
    let directive = match intent {
        InterpretIntent::ContinueThinking => CONTINUE_THINKING_DIRECTIVE,
        InterpretIntent::BreakOpen => BREAK_OPEN_DIRECTIVE,
    };
    format!("{INTERPRETER_SYSTEM_PROMPT_BASE}\n\n{directive}")
}

/// Render the user prompt from the compact context JSON + the utterance.
fn render_user_prompt(context_json: &str, utterance: &Utterance, budget: ContextBudget) -> String {
    let mut text = utterance.text.clone();
    if text.chars().count() > budget.max_transcript_chars {
        text = text.chars().take(budget.max_transcript_chars).collect();
    }
    format!(
        "CURRENT MAP CONTEXT (bounded):\n{context_json}\n\nLATEST UTTERANCE (utterance_id={}):\n{text}\n\nReturn the JSON operations object now.",
        utterance.utterance_id
    )
}

// ── Tolerant response parsing (ONE repair attempt, NO extra LLM call) ─────────

/// Parse the LLM completion into an [`InterpretationResponse`], tolerating code
/// fences and leading/trailing prose. Attempts, in order:
/// 1. Direct parse of the trimmed string.
/// 2. Strip ```json / ``` fences and parse.
/// 3. Extract the outermost `{...}` object and parse.
///
/// Any remaining failure is an error (NO second LLM call).
fn parse_response(raw: &str) -> anyhow::Result<InterpretationResponse> {
    let trimmed = raw.trim();

    // 1. Direct.
    if let Ok(resp) = serde_json::from_str::<InterpretationResponse>(trimmed) {
        return Ok(resp);
    }

    // 2. Strip code fences.
    let defenced = strip_code_fences(trimmed);
    if defenced != trimmed {
        if let Ok(resp) = serde_json::from_str::<InterpretationResponse>(defenced.trim()) {
            return Ok(resp);
        }
    }

    // 3. Extract outermost {...}.
    if let Some(obj) = extract_outermost_object(defenced) {
        if let Ok(resp) = serde_json::from_str::<InterpretationResponse>(obj) {
            return Ok(resp);
        }
    }

    Err(anyhow::anyhow!(
        "interpreter: could not parse model response as InterpretationResponse"
    ))
}

/// Strip a single leading/trailing markdown code fence (```json ... ``` or
/// ``` ... ```). Returns the inner content if a fence pair is found, else the
/// input unchanged.
fn strip_code_fences(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```") {
        // Drop an optional language tag on the first line.
        let after_lang = match rest.find('\n') {
            Some(nl) => &rest[nl + 1..],
            None => rest,
        };
        if let Some(inner) = after_lang.rfind("```") {
            return after_lang[..inner].trim();
        }
        return after_lang.trim();
    }
    s
}

/// Return the substring from the first `{` to its matching `}` (respecting
/// nested braces but not brace characters inside strings — adequate for the
/// well-formed-JSON-plus-prose case this repairs).
fn extract_outermost_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        let c = b as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            },
            _ => {},
        }
    }
    None
}

// ── Interpret ─────────────────────────────────────────────────────────────────

/// Interpret an utterance against the current map, returning a model-authored
/// envelope (or `None` for a valid zero-move interpretation).
///
/// `intent` steers the system prompt: `continue_thinking` keeps the map
/// incremental (0–2 moves), `break_open` asks for 2–4 complementary directions.
/// It only guides the model — the interpreter does not itself cap the count.
///
/// `applied_at` is the injected RFC3339 clock (deterministic — the interpreter
/// never calls `Utc::now()`). `trace_id` labels the model actor for provenance.
///
/// The produced envelope's `idempotency_key` is `interp:<utterance_id>`, making
/// re-interpretation of the same utterance idempotent at the store layer.
///
/// `progress` narrates the pipeline's stages for whoever is watching —
/// best-effort, never load-bearing. Callers with no audience pass
/// [`NoProgress`]; the terminal `Idle` is the caller's to emit once the
/// envelope has actually landed (or failed to).
pub async fn interpret(
    map: &ThinkingMap,
    utterance: &Utterance,
    intent: InterpretIntent,
    llm: &dyn InterpreterLlm,
    applied_at: &str,
    trace_id: Option<String>,
    progress: &dyn InterpretProgressSink,
) -> anyhow::Result<Option<MapOperationEnvelope>> {
    let budget = ContextBudget::default();

    // 1. Bounded context. Narrated with the live-node count — emitted after
    //    the build (which is a pure in-memory pass) so the count is real
    //    rather than estimated.
    let context = build_context(map, &utterance.text, budget);
    progress.stage(InterpretStage::Preparing, Some(context.digest.live_nodes));
    let context_json = serde_json::to_string(&context)?;

    // 2. Prompt (intent-directed: continue-thinking vs break-open).
    progress.stage(InterpretStage::LoadingContext, None);
    let system = system_prompt(intent);
    let user = render_user_prompt(&context_json, utterance, budget);

    // 3. LLM call — the stage that takes the seconds.
    progress.stage(InterpretStage::Facilitating, None);
    let raw = llm.complete(&system, &user).await?;

    // 4. Tolerant parse (ONE repair attempt, NO second LLM call).
    progress.stage(InterpretStage::Parsing, None);
    let resp = parse_response(&raw)?;

    // 5. Translate to canonical ops (stamps model_inferred + provisional + SourceRef).
    progress.stage(InterpretStage::Shaping, None);
    let ops = translate(&resp.operations, map, utterance, applied_at);

    // 6. Zero-move interpretation is valid — nothing to apply.
    if ops.is_empty() {
        return Ok(None);
    }

    // 7. Wrap in a Model-actor envelope (the reducer authority-validates it).
    let mut envelope = MapOperationEnvelope::new(
        new_envelope_id(),
        map.map_id.clone(),
        map.revision,
        OperationActor::Model { trace_id },
        format!("interp:{}", utterance.utterance_id),
        ops,
        applied_at,
    );
    envelope.utterance_id = Some(utterance.utterance_id.clone());

    Ok(Some(envelope))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thinking_map::models::ThinkingMapSource;
    use crate::thinking_map::reducer::{apply_envelope, ApplyOutcome};

    const TS: &str = "2026-07-19T00:00:00Z";
    const AT: &str = "2026-07-19T01:00:00Z";

    struct FakeLlm(String);
    #[async_trait]
    impl InterpreterLlm for FakeLlm {
        async fn complete(&self, _system: &str, _user: &str) -> anyhow::Result<String> {
            Ok(self.0.clone())
        }
    }

    /// An LLM that records the `system`/`user` prompts it was handed, so a test
    /// can assert what directive the interpreter built for a given intent.
    struct CapturingLlm {
        response: String,
        seen_system: std::sync::Mutex<Option<String>>,
        seen_user: std::sync::Mutex<Option<String>>,
    }
    impl CapturingLlm {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
                seen_system: std::sync::Mutex::new(None),
                seen_user: std::sync::Mutex::new(None),
            }
        }
    }
    #[async_trait]
    impl InterpreterLlm for CapturingLlm {
        async fn complete(&self, system: &str, user: &str) -> anyhow::Result<String> {
            *self.seen_system.lock().unwrap() = Some(system.to_string());
            *self.seen_user.lock().unwrap() = Some(user.to_string());
            Ok(self.response.clone())
        }
    }

    fn empty_map() -> ThinkingMap {
        ThinkingMap::new(
            "map-1".to_string(),
            "anonymous",
            "default",
            "Test",
            ThinkingMapSource::Solo,
            TS,
        )
    }

    fn utterance() -> Utterance {
        Utterance {
            utterance_id: "utt-1".to_string(),
            text: "Ship v1, but hiring too early is a risk".to_string(),
            thread_id: Some("thread-9".to_string()),
            timestamp: Some(TS.to_string()),
        }
    }

    #[tokio::test]
    async fn interpret_happy_path_and_reducer_applies() {
        let map = empty_map();
        let json = r#"{"operations":[
            {"op":"add_node","temp_id":"n1","kind":"idea","label":"Ship v1"},
            {"op":"add_node","temp_id":"n2","kind":"risk","label":"Hiring too early"},
            {"op":"connect","from":"n1","to":"n2","kind":"related_to"}
        ]}"#;
        let llm = FakeLlm(json.to_string());

        let env = interpret(
            &map,
            &utterance(),
            InterpretIntent::ContinueThinking,
            &llm,
            AT,
            Some("tr-1".to_string()),
            &NoProgress,
        )
        .await
        .unwrap()
        .expect("expected an envelope");

        // Actor = Model.
        assert!(matches!(env.actor, OperationActor::Model { .. }));
        // Idempotency key + utterance id.
        assert_eq!(env.idempotency_key, "interp:utt-1");
        assert_eq!(env.utterance_id, Some("utt-1".to_string()));

        // Collect the two minted node ids and assert provenance stamping.
        let mut minted_ids = Vec::new();
        for op in &env.operations {
            if let MapOperation::AddNode { node } = op {
                assert_eq!(node.assertion_origin, AssertionOrigin::ModelInferred);
                assert_eq!(node.epistemic_state, EpistemicState::Provisional);
                // SourceRef cites the utterance.
                assert_eq!(node.source_refs.len(), 1);
                let sr = &node.source_refs[0];
                assert_eq!(sr.utterance_id, Some("utt-1".to_string()));
                assert_eq!(sr.thread_id, Some("thread-9".to_string()));
                assert!(sr.quote.as_ref().unwrap().contains("Ship v1"));
                assert_eq!(sr.timestamp, Some(TS.to_string()));
                minted_ids.push(node.node_id.clone());
            }
        }
        assert_eq!(minted_ids.len(), 2);

        // The connect resolved both temp ids to the minted node ids.
        let connect = env
            .operations
            .iter()
            .find_map(|op| match op {
                MapOperation::Connect { edge } => Some(edge),
                _ => None,
            })
            .expect("a connect op");
        assert!(minted_ids.contains(&connect.from_node));
        assert!(minted_ids.contains(&connect.to_node));
        assert_ne!(connect.from_node, connect.to_node);
        assert_eq!(connect.assertion_origin, AssertionOrigin::ModelInferred);

        // Feed to the reducer at rev 0 — both add_nodes precede the connect, so
        // it applies cleanly with NO authority violation.
        match apply_envelope(&map, &env, AT).unwrap() {
            ApplyOutcome::Applied {
                map: new_map,
                resulting_revision,
                ..
            } => {
                assert_eq!(resulting_revision, 1);
                assert_eq!(new_map.nodes.len(), 2);
                assert_eq!(new_map.edges.len(), 1);
            },
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn model_cannot_confirm_only_op_dropped() {
        let map = empty_map();
        let json = r#"{"operations":[
            {"op":"set_epistemic_state","node_id":"n1","state":"confirmed"}
        ]}"#;
        let llm = FakeLlm(json.to_string());
        let out = interpret(
            &map,
            &utterance(),
            InterpretIntent::default(),
            &llm,
            AT,
            None,
            &NoProgress,
        )
        .await
        .unwrap();
        // The single confirmed op is dropped → zero moves → None.
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn model_confirm_dropped_but_others_kept() {
        let map = empty_map();
        let json = r#"{"operations":[
            {"op":"add_node","temp_id":"n1","kind":"idea","label":"Keep me"},
            {"op":"set_epistemic_state","node_id":"n1","state":"confirmed"}
        ]}"#;
        let llm = FakeLlm(json.to_string());
        let env = interpret(
            &map,
            &utterance(),
            InterpretIntent::default(),
            &llm,
            AT,
            None,
            &NoProgress,
        )
        .await
        .unwrap()
        .expect("env");
        // Only the add_node survives; no set_epistemic_state present.
        assert_eq!(env.operations.len(), 1);
        assert!(env
            .operations
            .iter()
            .all(|op| !matches!(op, MapOperation::SetEpistemicState { .. })));
    }

    #[tokio::test]
    async fn zero_move_returns_none() {
        let map = empty_map();
        let llm = FakeLlm(r#"{"operations":[]}"#.to_string());
        let out = interpret(
            &map,
            &utterance(),
            InterpretIntent::default(),
            &llm,
            AT,
            None,
            &NoProgress,
        )
        .await
        .unwrap();
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn tolerant_parse_fences_and_prose() {
        let map = empty_map();
        let raw = "Sure! Here are the operations:\n```json\n{\"operations\":[{\"op\":\"add_node\",\"temp_id\":\"n1\",\"kind\":\"fact\",\"label\":\"A fact\"}]}\n```\nHope that helps.";
        let llm = FakeLlm(raw.to_string());
        let env = interpret(
            &map,
            &utterance(),
            InterpretIntent::default(),
            &llm,
            AT,
            None,
            &NoProgress,
        )
        .await
        .unwrap()
        .expect("env");
        assert_eq!(env.operations.len(), 1);
    }

    #[tokio::test]
    async fn malformed_beyond_repair_errors() {
        let map = empty_map();
        let llm = FakeLlm("this is not json at all { oops".to_string());
        let err = interpret(
            &map,
            &utterance(),
            InterpretIntent::default(),
            &llm,
            AT,
            None,
            &NoProgress,
        )
        .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn idempotency_key_format() {
        let map = empty_map();
        let llm = FakeLlm(
            r#"{"operations":[{"op":"add_node","temp_id":"n1","kind":"idea","label":"x"}]}"#
                .to_string(),
        );
        let u = Utterance {
            utterance_id: "abc-123".to_string(),
            text: "hello".to_string(),
            thread_id: None,
            timestamp: None,
        };
        let env = interpret(
            &map,
            &u,
            InterpretIntent::default(),
            &llm,
            AT,
            None,
            &NoProgress,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(env.idempotency_key, "interp:abc-123");
        assert_eq!(env.utterance_id, Some("abc-123".to_string()));
    }

    #[tokio::test]
    async fn produced_envelope_passes_reducer_authority() {
        // A batch of model-safe ops; every add is model_inferred + provisional,
        // so no OriginMismatch / AuthorityViolation. (create_clarification is
        // omitted here because it would reference a not-yet-real temp id; that op
        // family's translation is covered by translate() unit coverage.)
        let map = empty_map();
        let json = r#"{"operations":[
            {"op":"add_node","temp_id":"n1","kind":"decision","label":"Adopt Rust","confidence":0.9},
            {"op":"add_node","temp_id":"n2","kind":"question","label":"Which runtime?"},
            {"op":"connect","from":"n1","to":"n2","kind":"leads_to"}
        ]}"#;
        let llm = FakeLlm(json.to_string());
        let env = interpret(
            &map,
            &utterance(),
            InterpretIntent::ContinueThinking,
            &llm,
            AT,
            Some("tr".to_string()),
            &NoProgress,
        )
        .await
        .unwrap()
        .expect("env");
        // Confidence clamped/kept in range.
        for op in &env.operations {
            if let MapOperation::AddNode { node } = op {
                assert!((0.0..=1.0).contains(&node.confidence));
            }
        }
        let outcome = apply_envelope(&map, &env, AT);
        assert!(
            matches!(outcome, Ok(ApplyOutcome::Applied { .. })),
            "expected clean apply, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn intent_directs_system_prompt() {
        let map = empty_map();
        // BreakOpen: the break-open directive must appear in the system prompt.
        let break_llm = CapturingLlm::new(r#"{"operations":[]}"#);
        let _ = interpret(
            &map,
            &utterance(),
            InterpretIntent::BreakOpen,
            &break_llm,
            AT,
            None,
            &NoProgress,
        )
        .await
        .unwrap();
        let break_system = break_llm.seen_system.lock().unwrap().clone().unwrap();
        let break_lower = break_system.to_lowercase();
        assert!(
            break_lower.contains("break open"),
            "break_open system prompt must mention 'break open': {break_system}"
        );
        assert!(
            break_lower.contains("complementary"),
            "break_open system prompt must mention 'complementary': {break_system}"
        );
        // The continue-thinking smallest-set directive must NOT be present.
        assert!(
            !break_lower.contains("smallest useful set"),
            "break_open must not carry the continue-thinking directive: {break_system}"
        );

        // ContinueThinking: the smallest-set directive must appear instead.
        let cont_llm = CapturingLlm::new(r#"{"operations":[]}"#);
        let _ = interpret(
            &map,
            &utterance(),
            InterpretIntent::ContinueThinking,
            &cont_llm,
            AT,
            None,
            &NoProgress,
        )
        .await
        .unwrap();
        let cont_system = cont_llm.seen_system.lock().unwrap().clone().unwrap();
        let cont_lower = cont_system.to_lowercase();
        assert!(
            cont_lower.contains("smallest useful set"),
            "continue_thinking system prompt must mention the smallest-set rule: {cont_system}"
        );
        assert!(
            !cont_lower.contains("break open"),
            "continue_thinking must not carry the break-open directive: {cont_system}"
        );
    }

    #[test]
    fn translate_preserves_order_and_clamps_confidence() {
        let map = empty_map();
        let proposals = vec![
            ProposedOperation::AddNode {
                temp_id: "a".to_string(),
                kind: NodeKind::Idea,
                label: "first".to_string(),
                detail: Some("d".to_string()),
                confidence: Some(5.0), // out of range → clamped to 1.0
            },
            ProposedOperation::TombstoneNode {
                node_id: "existing".to_string(),
            },
        ];
        let ops = translate(&proposals, &map, &utterance(), AT);
        assert_eq!(ops.len(), 2);
        match &ops[0] {
            MapOperation::AddNode { node } => {
                assert_eq!(node.confidence, 1.0);
                assert_eq!(node.detail_markdown, Some("d".to_string()));
                assert_eq!(node.created_at, AT);
            },
            other => panic!("expected AddNode first, got {other:?}"),
        }
        assert!(matches!(ops[1], MapOperation::TombstoneNode { .. }));
    }

    #[test]
    fn translate_drops_self_loop_connect() {
        let map = empty_map();
        // A connect where from == to after resolution (both literal "x").
        let proposals = vec![ProposedOperation::Connect {
            from: "x".to_string(),
            to: "x".to_string(),
            kind: EdgeKind::RelatedTo,
        }];
        let ops = translate(&proposals, &map, &utterance(), AT);
        assert!(ops.is_empty(), "self-loop connect must be dropped");
    }

    // ── Progress narration ───────────────────────────────────────────────────
    //
    // Each stage is bound to a real boundary in interpret(). These pin the
    // sequence so a stage cannot narrate work that does not happen — which is
    // how the client-side labels this replaces came to spend a year unset.

    /// Records every stage handed to it, in order.
    struct RecordingSink(std::sync::Mutex<Vec<(InterpretStage, Option<usize>)>>);

    impl RecordingSink {
        fn new() -> Self {
            Self(std::sync::Mutex::new(Vec::new()))
        }
        fn stages(&self) -> Vec<(InterpretStage, Option<usize>)> {
            self.0.lock().unwrap().clone()
        }
    }

    impl InterpretProgressSink for RecordingSink {
        fn stage(&self, stage: InterpretStage, node_count: Option<usize>) {
            self.0.lock().unwrap().push((stage, node_count));
        }
    }

    /// An LLM that fails, so the run dies at the facilitating boundary.
    struct FailingLlm;
    #[async_trait]
    impl InterpreterLlm for FailingLlm {
        async fn complete(&self, _system: &str, _user: &str) -> anyhow::Result<String> {
            Err(anyhow::anyhow!("model unavailable"))
        }
    }

    #[tokio::test]
    async fn progress_narrates_every_stage_in_pipeline_order() {
        let map = empty_map();
        let json = r#"{"operations":[{"op":"add_node","temp_id":"n1","kind":"idea","label":"x"}]}"#;
        let llm = FakeLlm(json.to_string());
        let sink = RecordingSink::new();
        let _ = interpret(
            &map,
            &utterance(),
            InterpretIntent::default(),
            &llm,
            AT,
            None,
            &sink,
        )
        .await
        .unwrap();

        let stages: Vec<InterpretStage> = sink.stages().iter().map(|(s, _)| *s).collect();
        assert_eq!(
            stages,
            vec![
                InterpretStage::Preparing,
                InterpretStage::LoadingContext,
                InterpretStage::Facilitating,
                InterpretStage::Parsing,
                InterpretStage::Shaping,
            ],
            "the five working stages, in pipeline order; Idle is the caller's"
        );
    }

    #[tokio::test]
    async fn preparing_carries_the_live_node_count() {
        // A map with two live nodes and one tombstoned: the count narrated is
        // what the facilitator actually reads, not the raw board size.
        let mut map = empty_map();
        for (id, dead) in [("a", false), ("b", false), ("c", true)] {
            map.nodes.insert(
                id.to_string(),
                ThinkingNode {
                    node_id: id.to_string(),
                    kind: NodeKind::Idea,
                    label: id.to_string(),
                    detail_markdown: None,
                    epistemic_state: EpistemicState::Provisional,
                    assertion_origin: AssertionOrigin::ModelInferred,
                    confidence: 0.5,
                    speaker: None,
                    source_refs: vec![],
                    parent_id: None,
                    position: None,
                    position_locked: false,
                    promoted_refs: vec![],
                    tombstoned: dead,
                    created_at: TS.to_string(),
                    updated_at: TS.to_string(),
                },
            );
        }
        let llm = FakeLlm(r#"{"operations":[]}"#.to_string());
        let sink = RecordingSink::new();
        let _ = interpret(
            &map,
            &utterance(),
            InterpretIntent::default(),
            &llm,
            AT,
            None,
            &sink,
        )
        .await
        .unwrap();

        let (stage, count) = sink.stages()[0];
        assert_eq!(stage, InterpretStage::Preparing);
        assert_eq!(count, Some(2), "two live nodes; the tombstone is not read");
        // No other stage claims a count — the number belongs to preparing.
        assert!(sink.stages()[1..].iter().all(|(_, n)| n.is_none()));
    }

    #[tokio::test]
    async fn llm_failure_stops_the_narration_at_facilitating() {
        let map = empty_map();
        let sink = RecordingSink::new();
        let out = interpret(
            &map,
            &utterance(),
            InterpretIntent::default(),
            &FailingLlm,
            AT,
            None,
            &sink,
        )
        .await;
        assert!(out.is_err());
        let stages: Vec<InterpretStage> = sink.stages().iter().map(|(s, _)| *s).collect();
        assert_eq!(
            stages,
            vec![
                InterpretStage::Preparing,
                InterpretStage::LoadingContext,
                InterpretStage::Facilitating,
            ],
            "a run that died in the model must not claim it parsed or shaped"
        );
    }

    #[tokio::test]
    async fn unparseable_response_stops_the_narration_at_parsing() {
        let map = empty_map();
        let llm = FakeLlm("this is not json at all { oops".to_string());
        let sink = RecordingSink::new();
        let out = interpret(
            &map,
            &utterance(),
            InterpretIntent::default(),
            &llm,
            AT,
            None,
            &sink,
        )
        .await;
        assert!(out.is_err());
        let stages: Vec<InterpretStage> = sink.stages().iter().map(|(s, _)| *s).collect();
        assert_eq!(
            *stages.last().unwrap(),
            InterpretStage::Parsing,
            "the failure happened in parsing, so parsing is where the story ends"
        );
        assert!(!stages.contains(&InterpretStage::Shaping));
    }

    /// The wire names are a contract with three clients; snake_case must hold.
    #[test]
    fn stage_wire_names_are_snake_case_and_stable() {
        assert_eq!(InterpretStage::Preparing.wire_name(), "preparing");
        assert_eq!(
            InterpretStage::LoadingContext.wire_name(),
            "loading_context"
        );
        assert_eq!(InterpretStage::Facilitating.wire_name(), "facilitating");
        assert_eq!(InterpretStage::Parsing.wire_name(), "parsing");
        assert_eq!(InterpretStage::Shaping.wire_name(), "shaping");
        assert_eq!(InterpretStage::Idle.wire_name(), "idle");
        // wire_name and serde agree, so an event built by hand and a stage
        // serialized directly can never spell the same stage differently.
        for stage in [
            InterpretStage::Preparing,
            InterpretStage::LoadingContext,
            InterpretStage::Facilitating,
            InterpretStage::Parsing,
            InterpretStage::Shaping,
            InterpretStage::Idle,
        ] {
            let json = serde_json::to_string(&stage).unwrap();
            assert_eq!(json, format!("\"{}\"", stage.wire_name()));
        }
    }
}
