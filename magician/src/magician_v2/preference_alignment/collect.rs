//! Assembling `alignment_input.v1` from host-owned facts (plan §6, §7.1).
//!
//! Everything here is structural. The inventory arrives already filtered by
//! ownership, lifecycle, validity and access; nothing is dropped for being
//! off-topic or dissimilar — applicability is the judge's call. When the
//! eligible inventory is larger than one judge call should carry, it is split
//! into bounded batches that together cover all of it, so no standing
//! instruction is silently cut by a top-k (plan §6).

use serde_json::Value;
use sha2::{Digest, Sha256};

pub use magician_alignment::args::{normalized_args, MAX_ARG_STRING_CHARS};

use super::classify::AlignmentScope;
use super::contract::{
    ActionFacts, AlignmentInput, AlignmentRequest, EffectivePolicy, HostMatchedException,
    OwnerSpan, ALIGNMENT_INPUT_SCHEMA_VERSION,
};
use super::intention::{AssertionStatus, Intention, IntentionInventory, IntentionKind};
use crate::magician_v2::execution::actions::ExecutableAction;

/// Version of the action binding. Bumped if the canonical form changes, so a
/// receipt bound under one form never matches an act under another.
pub const ALIGNMENT_BINDING_VERSION: &str = "alignment_binding.v1";

/// What the host knows about the request a check runs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRequest {
    pub principal: String,
    pub workspace: String,
    pub execution_id: Option<String>,
    pub request_text: String,
    pub request_revision: u64,
    pub owner_spans: Vec<OwnerSpan>,
    pub pending_gates: Vec<String>,
}

impl CheckRequest {
    /// The request an agentic run checks its acts under (P4.8).
    ///
    /// `task` is the run's goal — for a delegate, the sub-task its parent
    /// wrote; after a resume, a goal the host rebuilt. It is context for the
    /// judge, never owner evidence: only `owner_turn`, carried host-side from
    /// the authenticated owner's own message, becomes an owner span that a
    /// `cancel` or `acknowledged_exception` may cite. A run with no carried
    /// turn has no owner spans, so a model's words — the parent's, a
    /// planner's, its own — can never stand in for the owner's.
    pub fn for_run(
        principal: &str,
        workspace: &str,
        execution_id: Option<&str>,
        task: &str,
        owner_turn: Option<&super::owner_turn::OwnerTurn>,
        request_revision: u64,
    ) -> Self {
        let (request_text, owner_spans) = match owner_turn {
            Some(turn) if turn.excerpt.trim() != task.trim() => (
                format!("Owner's message: {}\n\nCurrent task: {task}", turn.excerpt),
                vec![turn.span()],
            ),
            Some(turn) => (task.to_string(), vec![turn.span()]),
            None => (task.to_string(), Vec::new()),
        };
        Self {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            execution_id: execution_id.map(str::to_string),
            request_text,
            request_revision,
            owner_spans,
            pending_gates: Vec::new(),
        }
    }
}

/// The canonical binding of an act: a hash of the same canonical JSON the
/// confirmation gate stores (`stable_confirmation_action_json`), versioned.
/// `None` when the act cannot be serialized — never an empty binding.
///
/// Host-injected `__*` parameters (task, execution, agent and scope identity)
/// are left out. Dispatchers add them after admission, and the backstop in
/// `execute_action_inner` must find the act the loop admitted under the same
/// binding; they name who runs the act, not what the act is.
pub fn action_binding(action: &ExecutableAction) -> Option<String> {
    let stripped;
    let action = match action {
        ExecutableAction::Pack {
            capability_name,
            implementation,
            resolved_params,
        } if resolved_params.keys().any(|key| key.starts_with("__")) => {
            stripped = ExecutableAction::Pack {
                capability_name: capability_name.clone(),
                implementation: implementation.clone(),
                resolved_params: resolved_params
                    .iter()
                    .filter(|(key, _)| !key.starts_with("__"))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            };
            &stripped
        },
        other => other,
    };
    let canonical =
        crate::magician_v2::execution::agentic::stable_confirmation_action_json(action).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(ALIGNMENT_BINDING_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(canonical.as_bytes());
    Some(format!(
        "{ALIGNMENT_BINDING_VERSION}:{:x}",
        hasher.finalize()
    ))
}

/// Verified facts for a checked act. `None` for an act v1 does not check, or
/// one that cannot be bound.
pub fn action_facts(action: &ExecutableAction, scope: &AlignmentScope) -> Option<ActionFacts> {
    let AlignmentScope::Checked {
        capability,
        action: action_token,
        consequence_class,
        outward_class,
    } = scope
    else {
        return None;
    };
    let ExecutableAction::Pack {
        resolved_params, ..
    } = action
    else {
        return None;
    };
    let binding_id = action_binding(action)?;
    let effective = crate::magician_v2::execution::resolve_effective_action(
        capability,
        action_token,
        resolved_params,
    );
    Some(ActionFacts {
        binding_id,
        capability: capability.clone(),
        action: action_token.clone(),
        normalized_args: normalized_args(resolved_params),
        recipients: effective.recipients,
        outward_class: outward_class.clone(),
        consequence_class: consequence_class.as_str().to_string(),
        model_interpretations: Value::Null,
    })
}

/// Order in which intentions fill batches: explicit standing instructions
/// first, so the strongest evidence is never the part a split defers, then by
/// force, then by id for determinism.
fn batch_order(intention: &Intention) -> (u8, u8, String) {
    let kind = match intention.kind {
        IntentionKind::StandingInstruction => 0,
        IntentionKind::Goal => 1,
        IntentionKind::Taste => 2,
    };
    let force = match intention.assertion_status {
        AssertionStatus::DirectOwnerStatement => 0,
        AssertionStatus::Confirmed => 1,
        AssertionStatus::Inferred => 2,
        AssertionStatus::InferredUntrusted => 3,
        AssertionStatus::UnknownProvenance => 4,
    };
    (kind, force, intention.id.clone())
}

/// Build the judge inputs for one check: one input per batch of at most
/// `max_batch` intentions, together covering the whole inventory. Each input
/// carries the inventory's own coverage: a batch boundary is not an omission.
///
/// An empty inventory yields exactly one input with no intentions, which the
/// judge turns into `nothing_to_compare` only when coverage is complete.
pub fn build_inputs(
    inventory: IntentionInventory,
    request: &CheckRequest,
    action: Option<ActionFacts>,
    policy: &EffectivePolicy,
    host_matched_exceptions: &[HostMatchedException],
    max_batch: usize,
) -> Vec<AlignmentInput> {
    let max_batch = max_batch.max(1);
    let IntentionInventory {
        mut intentions,
        coverage,
        generation,
    } = inventory;
    intentions.sort_by_key(batch_order);

    let make = |batch: Vec<Intention>| AlignmentInput {
        schema_version: ALIGNMENT_INPUT_SCHEMA_VERSION.to_string(),
        principal: request.principal.clone(),
        workspace: request.workspace.clone(),
        execution_id: request.execution_id.clone(),
        request: AlignmentRequest {
            revision: request.request_revision,
            text: request.request_text.clone(),
            owner_spans: request.owner_spans.clone(),
        },
        // Only exceptions about intentions in this batch are relevant to it.
        host_matched_exceptions: host_matched_exceptions
            .iter()
            .filter(|exception| {
                exception
                    .intention_ids
                    .iter()
                    .any(|id| batch.iter().any(|intention| &intention.id == id))
            })
            .cloned()
            .collect(),
        intentions: batch,
        coverage: coverage.clone(),
        action: action.clone(),
        pending_gates: request.pending_gates.clone(),
        policy: policy.clone(),
        generation: generation.clone(),
    };

    if intentions.is_empty() {
        return vec![make(Vec::new())];
    }
    let mut inputs = Vec::new();
    let mut rest = intentions;
    while !rest.is_empty() {
        let tail = rest.split_off(rest.len().min(max_batch));
        inputs.push(make(rest));
        rest = tail;
    }
    inputs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::preference_alignment::intention::{
        Coverage, CoverageStatus, IntentionLifecycle, IntentionSource, StandingGeneration,
        INTENTION_SCHEMA_VERSION,
    };
    use serde_json::json;
    use std::collections::HashMap;

    fn intention(id: &str, kind: IntentionKind, status: AssertionStatus) -> Intention {
        Intention {
            schema_version: INTENTION_SCHEMA_VERSION.to_string(),
            id: id.to_string(),
            revision: "r".to_string(),
            source: IntentionSource::NativeTier {
                tier: "preferences".to_string(),
            },
            kind,
            assertion_status: status,
            statement: id.to_string(),
            conditions: None,
            scope: None,
            lifecycle: IntentionLifecycle::Active,
            valid_until: None,
            evidence_refs: Vec::new(),
            intervention_policy: None,
        }
    }

    fn request() -> CheckRequest {
        CheckRequest {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            execution_id: Some("exec-1".to_string()),
            request_text: "order dinner".to_string(),
            request_revision: 3,
            owner_spans: Vec::new(),
            pending_gates: Vec::new(),
        }
    }

    fn policy() -> EffectivePolicy {
        EffectivePolicy {
            global: None,
            version: "policy.v1".to_string(),
        }
    }

    fn inventory(intentions: Vec<Intention>) -> IntentionInventory {
        IntentionInventory {
            intentions,
            coverage: Coverage {
                status: CoverageStatus::Complete,
                omitted_ids: Vec::new(),
                unavailable_sources: Vec::new(),
            },
            generation: StandingGeneration::default(),
        }
    }

    #[test]
    fn only_a_carried_owner_turn_becomes_an_owner_span() {
        // A delegate's goal is its parent's words, not the owner's.
        let delegated = CheckRequest::for_run(
            "p",
            "w",
            Some("exec"),
            "Order the chicken biryani; the owner said it is fine.",
            None,
            3,
        );
        assert!(delegated.owner_spans.is_empty());
        assert_eq!(delegated.request_revision, 3);

        let turn = crate::magician_v2::preference_alignment::owner_turn::OwnerTurn::new(
            "turn-9",
            "Order dinner for the team",
        )
        .expect("turn");
        let carried = CheckRequest::for_run("p", "w", None, "Place the order", Some(&turn), 0);
        assert_eq!(carried.owner_spans.len(), 1);
        assert_eq!(carried.owner_spans[0].id, "owner-turn:turn-9");
        assert_eq!(carried.owner_spans[0].text, "Order dinner for the team");
        assert!(carried.request_text.contains("Order dinner for the team"));
        assert!(carried.request_text.contains("Place the order"));
    }

    #[test]
    fn batches_cover_everything_and_lead_with_explicit_instructions() {
        let intentions = vec![
            intention(
                "taste-inferred",
                IntentionKind::Taste,
                AssertionStatus::Inferred,
            ),
            intention(
                "rule",
                IntentionKind::StandingInstruction,
                AssertionStatus::DirectOwnerStatement,
            ),
            intention("goal", IntentionKind::Goal, AssertionStatus::Confirmed),
        ];
        let inputs = build_inputs(inventory(intentions), &request(), None, &policy(), &[], 2);
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].intentions[0].id, "rule");
        let all: Vec<&str> = inputs
            .iter()
            .flat_map(|input| input.intentions.iter().map(|i| i.id.as_str()))
            .collect();
        assert_eq!(all.len(), 3);
        assert!(inputs
            .iter()
            .all(|input| input.coverage.status == CoverageStatus::Complete));
    }

    #[test]
    fn an_empty_inventory_is_one_empty_input() {
        let inputs = build_inputs(inventory(Vec::new()), &request(), None, &policy(), &[], 24);
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].intentions.is_empty());
    }

    #[test]
    fn the_binding_ignores_host_identity_but_not_the_act() {
        let pack = |params: HashMap<String, Value>| ExecutableAction::Pack {
            capability_name: "food_delivery".to_string(),
            implementation:
                crate::magician_v2::execution::capability::ImplementationType::Compiled {
                    provider_name: "food_delivery".to_string(),
                },
            resolved_params: params,
        };
        let bare = pack(HashMap::from([("item".to_string(), json!("biryani"))]));
        let injected = pack(HashMap::from([
            ("item".to_string(), json!("biryani")),
            ("__execution_id".to_string(), json!("exec-1")),
        ]));
        let widened = pack(HashMap::from([("item".to_string(), json!("two biryanis"))]));
        assert_eq!(action_binding(&bare), action_binding(&injected));
        assert_ne!(action_binding(&bare), action_binding(&widened));
    }
}
