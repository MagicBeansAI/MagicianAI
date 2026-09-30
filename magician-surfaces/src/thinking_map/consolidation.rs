//! Live Thinking Map — restructure consolidation (Phase 3, second slice).
//!
//! "The model proposes bounded operations; deterministic code owns state."
//!
//! A *sibling* of [`super::interpreter`]: where the interpreter turns ONE
//! utterance into an immediately-applied model-authored envelope, consolidation
//! reads the WHOLE board digest and asks the model to propose a **reorganization
//! of the board** — merge duplicates, group weakly-related nodes under a new
//! `group`, connect orphans, sharpen unclear labels. The proposed ops are NOT
//! applied directly. Instead they are wrapped in a
//! [`RestructureProposal`](super::models::RestructureProposal) authored by the
//! Model actor and staged (via `propose_restructure`) as a PENDING proposal.
//! Nothing on the board changes until the OWNER confirms — that owner confirm is
//! a separate, deterministic reducer path (`confirm_restructure`) that re-applies
//! the proposal's inner ops under the stored proposer's (Model) authority.
//!
//! This mirrors the interpreter precisely and REUSES its machinery: the same
//! [`InterpreterLlm`] trait, the same model-safe [`ProposedOperation`] subset,
//! the same deterministic [`translate`] (minting ids, stamping `model_inferred`
//! + `provisional` + a [`SourceRef`]), and the same bounded [`build_context`].
//! The only new surface is the consolidation system prompt, a
//! [`ConsolidationResponse`] envelope, and the `propose_restructure` wrapping.
//!
//! Deterministic where it counts: [`consolidate`] takes an injected `applied_at`
//! and never calls `Utc::now()`; the only non-determinism is id minting (uuid)
//! and the LLM itself.

use serde::{Deserialize, Serialize};

use super::context::{build_context, ContextBudget};
use super::interpreter::{translate, InterpreterLlm, ProposedOperation, Utterance};
use super::models::{new_proposal_id, ProposalState, RestructureProposal, ThinkingMap};
use super::operations::{new_envelope_id, MapOperation, MapOperationEnvelope, OperationActor};

/// The whole LLM response for a consolidation pass: a rationale (why the board is
/// being reorganized) plus the batch of proposed operations and the ids they
/// affect. Uses the SAME model-safe [`ProposedOperation`] subset as the
/// interpreter — the model literally cannot express an owner-only op.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsolidationResponse {
    /// Human-readable justification for the reorganization, surfaced to the owner
    /// on the confirm/reject surface and carried as the proposal's `rationale`.
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub operations: Vec<ProposedOperation>,
    /// The existing node ids this reorganization touches (advisory metadata for
    /// the owner-facing diff; not authority-bearing).
    #[serde(default)]
    pub affected_node_ids: Vec<String>,
}

// ── Prompt ───────────────────────────────────────────────────────────────────

/// Inline versioned consolidation system prompt. Instructs the model to propose
/// the SMALLEST high-signal reorganization (or none), using the SAME
/// [`ProposedOperation`] schema the interpreter offers. Migrating to a managed
/// prompt file is a follow-on; an inline `const` keeps this slice self-contained
/// (no config/file touches).
const CONSOLIDATION_SYSTEM_PROMPT_V1: &str = r#"You reorganize a Live Thinking Map. The map is a typed, revisable graph of the
user's thinking: ideas, facts, questions, decisions, options, risks, actions,
metrics, assumptions, evidence, and groups are first-class nodes; edges relate
them.

Given the CURRENT MAP CONTEXT (a bounded slice + an aggregate digest of the whole
board), propose a reorganization that makes the board clearer. Look for:
- DUPLICATES / near-duplicates to merge (relabel one to the canonical wording and
  connect the others to it, or group them).
- Weakly-related nodes that belong together — GROUP them under a NEW `group`
  node.
- ORPHANS (unconnected nodes) that should connect to a related node.
- UNCLEAR labels to sharpen (update_node with a clearer label).

Propose the SMALLEST high-signal reorganization. If the board is already
well-organized, return an EMPTY operations list — that is correct and expected.

Do NOT delete content destructively; prefer GROUPING and RELABELING over removal.
To group nodes: add_node a `group` node, then connect the members to it with
`grouped_under` edges (the group is the `to`, each member the `from`), or
move_to_parent the members under it. Never confirm nodes or move/position them —
those are the owner's alone and are not expressible here.

You MUST respond with ONLY a JSON object of this exact shape and nothing else:
{"rationale":"why this reorganization","operations":[ ... ],"affected_node_ids":["<existing id>", ...]}

Each operation is one of (the `op` field selects the variant), the SAME schema
the interpreter uses:
- {"op":"add_node","temp_id":"g1","kind":"idea|fact|question|decision|option|risk|action|metric|assumption|evidence|group","label":"short label","detail":"optional markdown","confidence":0.0-1.0}
- {"op":"update_node","node_id":"<existing id>","label":"...","detail":"...","confidence":0.0-1.0}
- {"op":"set_node_kind","node_id":"<existing id>","kind":"..."}
- {"op":"set_epistemic_state","node_id":"<existing id>","state":"provisional|asserted|contradicted|rejected|resolved|superseded"}
- {"op":"tombstone_node","node_id":"<existing id>"}
- {"op":"connect","from":"<node_id or temp_id>","to":"<node_id or temp_id>","kind":"related_to|supports|contradicts|answers|depends_on|leads_to|alternative_to|measures|grouped_under"}
- {"op":"create_clarification","node_id":"<existing id>","question":"..."}

Rules:
- `temp_id` is your own handle for a node you create in THIS batch; reference it
  from a `connect` in the same batch. To reference an EXISTING node, use its real
  `node_id` from the context.
- You may NOT confirm a node (there is no "confirmed" state available to you) and
  you may NOT move nodes, change the shared view, or promote nodes — those are the
  owner's alone and are not expressible here.
- Prefer few, high-signal operations. Keep labels short. Do not invent node ids
  that are not in the context.
- Respond with the JSON object only. No prose, no code fences."#;

/// Render the user prompt from the compact context JSON. Consolidation has no
/// single utterance — the digest + node lists ARE the input.
fn render_user_prompt(context_json: &str) -> String {
    format!(
        "CURRENT MAP CONTEXT (bounded, whole-board digest):\n{context_json}\n\nPropose the reorganization JSON object now (empty operations if already well-organized).",
    )
}

// ── Tolerant response parsing (ONE repair attempt, NO extra LLM call) ─────────

/// Parse the LLM completion into a [`ConsolidationResponse`], tolerating code
/// fences and leading/trailing prose. Attempts, in order:
/// 1. Direct parse of the trimmed string.
/// 2. Strip ```json / ``` fences and parse.
/// 3. Extract the outermost `{...}` object and parse.
///
/// Any remaining failure is an error (NO second LLM call). This mirrors the
/// interpreter's `parse_response` — duplicated here (rather than exported) so the
/// two modules stay independently evolvable.
fn parse_response(raw: &str) -> anyhow::Result<ConsolidationResponse> {
    let trimmed = raw.trim();

    // 1. Direct.
    if let Ok(resp) = serde_json::from_str::<ConsolidationResponse>(trimmed) {
        return Ok(resp);
    }

    // 2. Strip code fences.
    let defenced = strip_code_fences(trimmed);
    if defenced != trimmed {
        if let Ok(resp) = serde_json::from_str::<ConsolidationResponse>(defenced.trim()) {
            return Ok(resp);
        }
    }

    // 3. Extract outermost {...}.
    if let Some(obj) = extract_outermost_object(defenced) {
        if let Ok(resp) = serde_json::from_str::<ConsolidationResponse>(obj) {
            return Ok(resp);
        }
    }

    Err(anyhow::anyhow!(
        "consolidation: could not parse model response as ConsolidationResponse"
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

/// Return the substring from the first `{` to its matching `}` (respecting nested
/// braces but not brace characters inside strings — adequate for the
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

// ── Consolidate ────────────────────────────────────────────────────────────────

/// Read the board, ask the model to propose a reorganization, and wrap it as a
/// model-authored [`MapOperation::ProposeRestructure`] envelope (or `None` if the
/// board is already well-organized).
///
/// The returned envelope, applied via the reducer's `propose_restructure`, stages
/// a PENDING proposal — NOTHING on the board changes until the owner confirms.
/// The reducer validates the proposal's inner ops against the `Model` actor at
/// propose time (and again at confirm), so the produced ops must be in the
/// model-safe subset — [`translate`] guarantees this by construction (it only
/// emits ops derived from the model-safe [`ProposedOperation`]).
///
/// `applied_at` is the injected RFC3339 clock (deterministic — this never calls
/// `Utc::now()`). `trace_id` labels the model actor for provenance. The produced
/// envelope's `idempotency_key` is `consolidate:<uuid>` (a fresh reorganization
/// each call — consolidation is not naturally idempotent on a fixed key).
pub async fn consolidate(
    map: &ThinkingMap,
    llm: &dyn InterpreterLlm,
    applied_at: &str,
    trace_id: Option<String>,
) -> anyhow::Result<Option<MapOperationEnvelope>> {
    let budget = ContextBudget::default();

    // 1. Bounded whole-board context (empty focus text — the digest + node lists
    //    are what matter, not one utterance).
    let context = build_context(map, "", budget);
    let context_json = serde_json::to_string(&context)?;

    // 2. Prompt.
    let system = CONSOLIDATION_SYSTEM_PROMPT_V1;
    let user = render_user_prompt(&context_json);

    // 3. LLM call.
    let raw = llm.complete(system, &user).await?;

    // 4. Tolerant parse (ONE repair attempt, NO second LLM call).
    let resp = parse_response(&raw)?;

    // 5. Board already well-organized ⇒ nothing to propose (valid).
    if resp.operations.is_empty() {
        return Ok(None);
    }

    // 6. Translate the proposed ops into canonical ops via the interpreter's
    //    deterministic translator, using a SYNTHETIC utterance for provenance:
    //    any newly-added group node gets a `model_inferred` origin + a SourceRef
    //    citing this consolidation.
    let consolidation_ref = format!("consolidate:{}", uuid::Uuid::new_v4());
    let synthetic_utterance = Utterance {
        utterance_id: consolidation_ref.clone(),
        text: resp.rationale.clone(),
        thread_id: None,
        timestamp: Some(applied_at.to_string()),
    };
    let inner_ops = translate(&resp.operations, map, &synthetic_utterance, applied_at);

    // Translation could in principle drop every op (e.g. a lone model `confirmed`
    // or self-loop) → an empty proposal the reducer would reject. Treat that as a
    // valid no-op consolidation.
    if inner_ops.is_empty() {
        return Ok(None);
    }

    // 7. Build the model-authored restructure proposal.
    let proposal = RestructureProposal {
        proposal_id: new_proposal_id(),
        proposed_by: OperationActor::Model {
            trace_id: trace_id.clone(),
        },
        rationale: resp.rationale,
        operations: inner_ops,
        state: ProposalState::Proposed,
        affected_node_ids: resp.affected_node_ids,
        created_at: applied_at.to_string(),
        resolved_at: None,
    };

    // 8. Wrap the single ProposeRestructure op in a Model-actor envelope (the
    //    reducer authority-validates it + its inner ops).
    let propose_op = MapOperation::ProposeRestructure { proposal };
    let envelope = MapOperationEnvelope::new(
        new_envelope_id(),
        map.map_id.clone(),
        map.revision,
        OperationActor::Model { trace_id },
        consolidation_ref,
        vec![propose_op],
        applied_at,
    );

    Ok(Some(envelope))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thinking_map::models::{
        AssertionOrigin, EpistemicState, NodeKind, ThinkingMapSource, ThinkingNode,
    };
    use crate::thinking_map::operations::MapOperationEnvelope;
    use crate::thinking_map::reducer::{apply_envelope, ApplyOutcome};
    use async_trait::async_trait;

    const TS: &str = "2026-07-19T00:00:00Z";
    const AT: &str = "2026-07-19T01:00:00Z";
    const AT2: &str = "2026-07-19T02:00:00Z";

    struct FakeLlm(String);
    #[async_trait]
    impl InterpreterLlm for FakeLlm {
        async fn complete(&self, _system: &str, _user: &str) -> anyhow::Result<String> {
            Ok(self.0.clone())
        }
    }

    fn map_with_node(node_id: &str) -> ThinkingMap {
        let mut map = ThinkingMap::new(
            "map-1".to_string(),
            "anonymous",
            "default",
            "Test",
            ThinkingMapSource::Solo,
            TS,
        );
        // An owner-spoken node the consolidation can group.
        map.nodes.insert(
            node_id.to_string(),
            ThinkingNode {
                node_id: node_id.to_string(),
                kind: NodeKind::Risk,
                label: "Hiring too early".to_string(),
                detail_markdown: None,
                epistemic_state: EpistemicState::Asserted,
                assertion_origin: AssertionOrigin::OwnerSpoken,
                confidence: 0.5,
                speaker: None,
                source_refs: vec![],
                parent_id: None,
                position: None,
                position_locked: false,
                promoted_refs: vec![],
                tombstoned: false,
                created_at: TS.to_string(),
                updated_at: TS.to_string(),
            },
        );
        map
    }

    #[tokio::test]
    async fn happy_path_stages_pending_proposal_then_owner_confirm_materializes() {
        let map = map_with_node("existing");
        // Group the existing risk under a new "Risks" group.
        let json = r#"{
            "rationale":"group the risks",
            "operations":[
                {"op":"add_node","temp_id":"g1","kind":"group","label":"Risks"},
                {"op":"connect","from":"existing","to":"g1","kind":"grouped_under"}
            ],
            "affected_node_ids":["existing"]
        }"#;
        let llm = FakeLlm(json.to_string());

        let env = consolidate(&map, &llm, AT, Some("tr-1".to_string()))
            .await
            .unwrap()
            .expect("expected a consolidation envelope");

        // The envelope carries exactly ONE ProposeRestructure op, authored by Model.
        assert!(matches!(env.actor, OperationActor::Model { .. }));
        assert_eq!(env.operations.len(), 1);
        let proposal = match &env.operations[0] {
            MapOperation::ProposeRestructure { proposal } => proposal,
            other => panic!("expected ProposeRestructure, got {other:?}"),
        };
        assert!(matches!(proposal.proposed_by, OperationActor::Model { .. }));
        assert_eq!(proposal.state, ProposalState::Proposed);
        assert_eq!(proposal.rationale, "group the risks");
        assert_eq!(proposal.affected_node_ids, vec!["existing".to_string()]);
        // Inner ops were translated: add_node (group) + connect.
        assert_eq!(proposal.operations.len(), 2);
        // The new group node is model-authored + provisional + cites the consolidation.
        let group_node = proposal.operations.iter().find_map(|op| match op {
            MapOperation::AddNode { node } => Some(node),
            _ => None,
        });
        let group_node = group_node.expect("an add_node for the group");
        assert_eq!(group_node.kind, NodeKind::Group);
        assert_eq!(group_node.assertion_origin, AssertionOrigin::ModelInferred);
        assert_eq!(group_node.epistemic_state, EpistemicState::Provisional);
        assert_eq!(group_node.source_refs.len(), 1);
        assert!(group_node.source_refs[0]
            .utterance_id
            .as_ref()
            .unwrap()
            .starts_with("consolidate:"));

        // Applying the envelope STAGES the proposal — it does NOT materialize the
        // inner ops. The group node is NOT in map.nodes yet.
        let staged = match apply_envelope(&map, &env, AT).unwrap() {
            ApplyOutcome::Applied { map, .. } => map,
            other => panic!("expected Applied, got {other:?}"),
        };
        assert_eq!(staged.proposals.len(), 1);
        let staged_proposal_id = staged.proposals.keys().next().unwrap().clone();
        assert_eq!(
            staged.proposals[&staged_proposal_id].state,
            ProposalState::Proposed
        );
        // Only the original node is present — the group is NOT yet materialized.
        assert_eq!(staged.nodes.len(), 1);
        assert!(staged.nodes.contains_key("existing"));
        assert_eq!(staged.edges.len(), 0);

        // Now the OWNER confirms → the inner ops materialize and the proposal
        // flips to Confirmed.
        let confirm = MapOperationEnvelope::new(
            new_envelope_id(),
            staged.map_id.clone(),
            staged.revision,
            OperationActor::Owner {
                principal: "anonymous".to_string(),
            },
            format!("proposal-decision:{staged_proposal_id}:confirm"),
            vec![MapOperation::ConfirmRestructure {
                proposal_id: staged_proposal_id.clone(),
            }],
            AT2,
        );
        let confirmed = match apply_envelope(&staged, &confirm, AT2).unwrap() {
            ApplyOutcome::Applied { map, .. } => map,
            other => panic!("expected Applied, got {other:?}"),
        };
        assert_eq!(
            confirmed.proposals[&staged_proposal_id].state,
            ProposalState::Confirmed
        );
        // The group node + the grouped_under edge now exist.
        assert_eq!(confirmed.nodes.len(), 2);
        assert_eq!(
            confirmed.edges.values().filter(|e| !e.tombstoned).count(),
            1
        );
        // The materialized group node stayed model-authored (NOT laundered to owner).
        let materialized_group = confirmed
            .nodes
            .values()
            .find(|n| n.kind == NodeKind::Group)
            .expect("the group node materialized");
        assert_eq!(
            materialized_group.assertion_origin,
            AssertionOrigin::ModelInferred
        );
    }

    #[tokio::test]
    async fn empty_operations_returns_none() {
        let map = map_with_node("existing");
        let llm = FakeLlm(r#"{"rationale":"already tidy","operations":[]}"#.to_string());
        let out = consolidate(&map, &llm, AT, None).await.unwrap();
        assert!(out.is_none(), "an empty reorganization must yield None");
    }

    #[tokio::test]
    async fn tolerant_parse_fences_and_prose() {
        let map = map_with_node("existing");
        let raw = "Sure! Here's a reorganization:\n```json\n{\"rationale\":\"group\",\"operations\":[{\"op\":\"add_node\",\"temp_id\":\"g1\",\"kind\":\"group\",\"label\":\"Risks\"}]}\n```\nHope that helps.";
        let llm = FakeLlm(raw.to_string());
        let env = consolidate(&map, &llm, AT, None)
            .await
            .unwrap()
            .expect("fenced JSON must parse");
        assert_eq!(env.operations.len(), 1);
        match &env.operations[0] {
            MapOperation::ProposeRestructure { proposal } => {
                assert_eq!(proposal.rationale, "group");
                assert_eq!(proposal.operations.len(), 1);
            },
            other => panic!("expected ProposeRestructure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_beyond_repair_errors() {
        let map = map_with_node("existing");
        let llm = FakeLlm("this is not json at all { oops".to_string());
        let err = consolidate(&map, &llm, AT, None).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn translation_drops_everything_returns_none() {
        // A lone model `confirmed` set_epistemic_state is dropped by translate() →
        // empty inner ops → a valid no-op consolidation (None), never an empty
        // proposal the reducer would reject.
        let map = map_with_node("existing");
        let json = r#"{"rationale":"x","operations":[{"op":"set_epistemic_state","node_id":"existing","state":"confirmed"}]}"#;
        let llm = FakeLlm(json.to_string());
        let out = consolidate(&map, &llm, AT, None).await.unwrap();
        assert!(out.is_none());
    }
}
