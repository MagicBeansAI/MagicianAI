//! Phase 3 approval gate interpreter.

use std::borrow::Cow;
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::types::{
    effective_tool_parameter_values, ActionPattern, AgentConstraints, AgentDefinition,
    ApprovalCondition, ApprovalRule,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionStep {
    pub id: String,
    pub goal: String,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub action_type: Option<String>,
    #[serde(default)]
    pub params: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub plan_id: String,
    #[serde(default)]
    pub steps: Vec<ExecutionStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub step_id: String,
    pub action_description: String,
    pub matched_rule: ApprovalRule,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApprovalResult {
    Approved(ExecutionPlan),
    NeedsApproval {
        plan: ExecutionPlan,
        pending_actions: Vec<PendingApproval>,
    },
}

#[derive(Debug, Default, Clone)]
pub struct ApprovalGate;

impl ApprovalGate {
    pub fn check(&self, plan: &ExecutionPlan, constraints: &AgentConstraints) -> ApprovalResult {
        let mut pending = Vec::new();

        for step in &plan.steps {
            for rule in &constraints.requires_approval {
                if rule_matches(rule, step) {
                    pending.push(PendingApproval {
                        step_id: step.id.clone(),
                        action_description: step.goal.clone(),
                        matched_rule: rule.clone(),
                    });
                    break;
                }
            }
        }

        if pending.is_empty() {
            ApprovalResult::Approved(plan.clone())
        } else {
            ApprovalResult::NeedsApproval {
                plan: plan.clone(),
                pending_actions: pending,
            }
        }
    }
}

fn rule_matches(rule: &ApprovalRule, step: &ExecutionStep) -> bool {
    if !tool_matches(rule.tool.as_str(), step.tool.as_deref()) {
        return false;
    }

    let step_action = step.action_type.as_deref().unwrap_or_default();
    if !rule.action.matches(step_action) {
        return false;
    }

    rule.when
        .as_ref()
        .map(|condition| condition_matches(condition, step))
        .unwrap_or(true)
}

// ── Shared `Pack`-action approval-routing helpers ───────────────────────
//
// These derive the rule-`tool` and rule-`action` tokens for a `Pack`
// (capability) action so an `ApprovalRule` can be matched against it. The
// agentic executor's confirmation gate uses the SAME derivation when it
// builds an `ExecutionStep` for a `Pack` action (see
// `execution::agentic::executor::approval_step_from_candidate`'s `Pack`
// arm, which delegates here). Centralizing them guarantees the chat
// compiled-tool dispatch fork and the executor gate route the identical
// set of tools — no drift.

/// Normalize a raw action discriminator (snake/camel/mixed) into the
/// lower_snake token form approval rules are authored against. Strips
/// any char that is not `[A-Za-z0-9_]` and inserts `_` at camelCase
/// boundaries.
pub fn normalize_approval_action_token(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(chars.len() + 8);

    for (idx, ch) in chars.iter().enumerate() {
        if !ch.is_ascii_alphanumeric() && *ch != '_' {
            continue;
        }

        if ch.is_ascii_uppercase() {
            let prev = idx.checked_sub(1).and_then(|i| chars.get(i)).copied();
            let next = chars.get(idx + 1).copied();
            let prev_is_separator = prev.is_some_and(|c| !c.is_ascii_alphanumeric() && c != '_');
            let prev_is_lower_or_digit =
                prev.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
            let prev_is_upper = prev.is_some_and(|c| c.is_ascii_uppercase());
            let next_is_lower = next.is_some_and(|c| c.is_ascii_lowercase());

            if !out.is_empty()
                && !out.ends_with('_')
                && !prev_is_separator
                && (prev_is_lower_or_digit || (prev_is_upper && next_is_lower))
            {
                out.push('_');
            }
        }

        out.push(ch.to_ascii_lowercase());
    }

    out
}

/// Derive the rule-`tool` token for a capability (`Pack`) action.
/// For a namespaced tool (`pack__action`) this is the pack prefix; for a
/// flat compiled tool name (e.g. `imessage_send`, no `__`) it's the name
/// itself.
pub fn pack_tool_for_approval(capability_name: &str) -> String {
    capability_name
        .split_once("__")
        .map(|(pack, _)| pack)
        .unwrap_or(capability_name)
        .to_string()
}

/// Derive the rule-`action` token for a capability (`Pack`) action.
/// Prefers an explicit discriminator param (`action` / `action_type` /
/// `operation` / `tool_name`), then the `__`-suffix, then `execute`.
pub fn pack_action_for_approval(
    capability_name: &str,
    resolved_params: &HashMap<String, Value>,
) -> String {
    for key in ["action", "action_type", "operation", "tool_name"] {
        if let Some(Value::String(action)) = resolved_params.get(key) {
            let normalized = normalize_approval_action_token(action.trim());
            if !normalized.is_empty() {
                return normalized;
            }
        }
    }
    if let Some((_, action)) = capability_name.split_once("__") {
        let normalized = normalize_approval_action_token(action.trim());
        if !normalized.is_empty() {
            return normalized;
        }
    }
    "execute".to_string()
}

/// Canonical approval coordinates for every provider-visible tool/control.
///
/// Structural controls are represented by the executor as actions on the
/// synthetic `orchestrator` tool.  Chat, voice, snapshot introspection and the
/// autonomous executor must all use this exact mapping; treating a structural
/// name as a flat capability (`delegate_to_agent.execute`) bypasses correctly
/// authored rules such as `orchestrator.delegate_to_agent`.
pub fn tool_action_for_approval(
    tool_name: &str,
    resolved_params: &HashMap<String, Value>,
) -> (String, String) {
    match tool_name {
        "spawn_sub_goal"
        | "delegate_to_agent"
        | "handover_to_agent"
        | "orchestrate_pipeline"
        | "yield"
        | "need_user_input"
        | "goal_reached"
        | "cannot_proceed"
        | "delegate_to_chat" => ("orchestrator".to_string(), tool_name.to_string()),
        _ => (
            pack_tool_for_approval(tool_name),
            pack_action_for_approval(tool_name, resolved_params),
        ),
    }
}

/// Returns whether an action requires approval using the canonical structural
/// or capability coordinates above.
pub fn agent_requires_approval_for_tool(
    constraints: &AgentConstraints,
    tool_name: &str,
    resolved_params: &HashMap<String, Value>,
) -> bool {
    if constraints.requires_approval.is_empty() {
        return false;
    }
    let (tool, action) = tool_action_for_approval(tool_name, resolved_params);
    let step = ExecutionStep {
        id: format!("approval-probe-{tool_name}"),
        goal: tool_name.to_string(),
        tool: Some(tool),
        action_type: Some(action),
        params: resolved_params.clone(),
    };
    constraints
        .requires_approval
        .iter()
        .any(|rule| rule_matches(rule, &step))
}

/// Returns `true` when the agent's `constraints.requires_approval` rules
/// would gate dispatch of the compiled/capability tool `tool_name` with
/// `resolved_params` — i.e. the executor's `ApprovalGate` would mark it
/// `NeedsApproval`. The chat fast-path uses this to decide whether to
/// route a compiled tool through the executor (which enforces the gate +
/// pause) instead of the bypass fast path. The derivation mirrors the
/// executor's `Pack`-action gate exactly so the two route the identical
/// set of tools.
pub fn agent_requires_approval_for_pack_tool(
    constraints: &AgentConstraints,
    tool_name: &str,
    resolved_params: &HashMap<String, Value>,
) -> bool {
    if constraints.requires_approval.is_empty() {
        return false;
    }
    let step = ExecutionStep {
        id: format!("approval-probe-{tool_name}"),
        goal: tool_name.to_string(),
        tool: Some(pack_tool_for_approval(tool_name)),
        action_type: Some(pack_action_for_approval(tool_name, resolved_params)),
        params: resolved_params.clone(),
    };
    constraints
        .requires_approval
        .iter()
        .any(|rule| rule_matches(rule, &step))
}

/// Conservative catalog-level projection for a structural action.  Conditions
/// depend on call arguments and therefore cannot be decided while building the
/// schema; matching tool/action coordinates are enough to mark the grant as
/// potentially approval-gated. Dispatch performs the exact conditional match.
pub fn structural_action_may_require_approval(rules: &[ApprovalRule], action_name: &str) -> bool {
    rules.iter().any(|rule| {
        tool_matches(rule.tool.as_str(), Some("orchestrator")) && rule.action.matches(action_name)
    })
}

/// One `requires_approval` rule per harness mutation tool
/// (`HARNESS_APPROVAL_GATED_TOOL_NAMES`). These are flat compiled tool names
/// (no `__`), so `pack_tool_for_approval(name) == name` and the action token
/// falls back to `execute`, matched by the `"*"` wildcard — see
/// `agent_requires_approval_gates_flat_compiled_tool`. Injected centrally for
/// every harness agent so autonomous roster/delegation mutations pause for the owner.
pub fn harness_mutation_approval_rules() -> Vec<ApprovalRule> {
    crate::magician_v2::harness::HARNESS_APPROVAL_GATED_TOOL_NAMES
        .iter()
        .map(|tool| ApprovalRule {
            tool: (*tool).to_string(),
            action: ActionPattern::Single("*".to_string()),
            when: None,
            ttl_secs: None,
        })
        .collect()
}

/// An agent's own `requires_approval` rules, plus — for a HARNESS agent — the
/// central mutation gate (`harness_mutation_approval_rules`), so autonomous
/// roster/delegation self-mutation pauses for owner approval regardless of the
/// agent's YAML. Deduped by tool name (a YAML rule for a gated tool wins / isn't
/// doubled). Call this EVERYWHERE `approval_rules` is built from a harness agent
/// definition (initial dispatch, owner-transition profile reload, chat binding),
/// so the gate holds wherever a harness owner profile is constructed.
pub fn harness_merged_approval_rules(definition: &AgentDefinition) -> Vec<ApprovalRule> {
    let mut rules = definition.constraints.requires_approval.clone();
    if definition.harness.is_some() {
        for rule in harness_mutation_approval_rules() {
            // Only skip the central blanket rule when an existing rule already
            // gates this tool for ALL actions (a "*" wildcard). A NARROWER YAML
            // rule (e.g. action "delete") must NOT suppress the "*" gate, or a
            // different action on a gated mutation tool would slip through
            // ungated. `action.matches("*")` is true iff the rule is itself a
            // wildcard.
            let already_blanket_gated = rules
                .iter()
                .any(|existing| existing.tool == rule.tool && existing.action.matches("*"));
            if !already_blanket_gated {
                rules.push(rule);
            }
        }
    }
    rules
}

fn tool_matches(rule_tool: &str, step_tool: Option<&str>) -> bool {
    rule_tool == "*" || step_tool == Some(rule_tool)
}

fn condition_matches(condition: &ApprovalCondition, step: &ExecutionStep) -> bool {
    let has_param_conditions = !condition.param_matches.is_empty();
    let has_url_conditions = !condition.url_contains.is_empty();
    if !has_param_conditions && !has_url_conditions {
        return true;
    }

    let tool_name = step.tool.as_deref().unwrap_or_default();
    let param_match = condition
        .param_matches
        .iter()
        .any(|(param_name, patterns)| {
            effective_tool_parameter_values(tool_name, &step.params, param_name)
                .into_iter()
                .filter_map(value_as_matchable_string)
                .any(|value| patterns.iter().any(|pattern| value.contains(pattern)))
        });

    let url_match = if has_url_conditions {
        effective_tool_parameter_values(tool_name, &step.params, "url")
            .into_iter()
            .filter_map(Value::as_str)
            .any(|url| {
                condition
                    .url_contains
                    .iter()
                    .any(|pattern| url.contains(pattern))
            })
    } else {
        false
    };

    param_match || url_match
}

fn value_as_matchable_string(value: &Value) -> Option<Cow<'_, str>> {
    match value {
        Value::String(s) => Some(Cow::Borrowed(s)),
        Value::Number(n) => Some(Cow::Owned(n.to_string())),
        Value::Bool(b) => Some(Cow::Owned(b.to_string())),
        _ => None,
    }
}

// ── Standing consent in front of the rules ──────────────────────────────
//
// `ApprovalGate::check` is the one place every approval ask converges: the five
// confirmation gates in the agentic executor and the pause-state reconstruction
// in `approval_service` all interpret `constraints.requires_approval` through
// it. So it is the only integration point for approval envelopes that
// generalises without being enumerated — see
// `crate::magician_v2::approval_envelopes::waiver`.

/// A standing consent an approval prompt may be waived against.
///
/// Carries only what the caller alone can know — which envelopes apply, which
/// posture is in force, and the facts no rule interpreter could look up. Every
/// property of the ACT is derived from the step being checked, never supplied
/// here: a caller that could describe the act could describe a softer one.
///
/// Absent (`None` at the call site) means **no standing consent is available**,
/// which asks about everything. That is the same answer as `Off`, and it is
/// deliberate that the two are indistinguishable to the gate: a caller that
/// forgot to build one must not thereby waive anything.
#[derive(Debug, Clone, Copy)]
pub struct StandingConsent<'a> {
    pub gate: &'a crate::magician_v2::approval_envelopes::EnvelopeGate,
    pub mode: crate::magician_v2::approval_envelopes::EnvelopeMode,
    /// The stable key the consumption debit is idempotent under, together with
    /// the step's own content.
    ///
    /// Blank or absent yields an empty act ref, which the envelope gate refuses
    /// as `unidentified_act`. That is the fail-closed answer: an act that cannot
    /// be debited idempotently would share one debit slot with every other such
    /// act, so an envelope for ten would authorise an unbounded number.
    pub act_scope: Option<&'a str>,
    pub envelope_scope: Option<&'a crate::magician_v2::approval_envelopes::EnvelopeScope>,
    pub engagement_id: Option<&'a str>,
    /// `&[]` is **not** "nobody to check against". The recipient predicate fails
    /// closed on a recipient it cannot find here, so an empty list refuses every
    /// recipient rather than passing vacuously.
    pub engagement_identities: &'a [String],
    /// `None` is unknown, not zero — see `ActFacts`.
    pub attachments_outside_ledger: Option<usize>,
    pub value_micros: Option<u64>,
    pub now: DateTime<Utc>,
    /// Read without debiting. For a surface asking *"would this pause?"* to
    /// decide what to render.
    ///
    /// **A preview must never be allowed to let an act through.** It resolves
    /// through shadow semantics, so its answer is advisory: between the preview
    /// and the real decision an envelope can expire, be revoked, or be spent by
    /// another act. A caller that proceeded on a preview would perform an act no
    /// ledger recorded, which is the one outcome that makes an envelope's limits
    /// decorative.
    pub preview_only: bool,
}

/// One prompt a standing consent removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaivedApproval {
    pub step_id: String,
    pub envelope_id: String,
    pub outcome: String,
    pub matched_predicates: Vec<String>,
    /// The line for the approval log. A prompt that vanished without a record is
    /// the thing that makes envelopes untrustworthy.
    pub audit_line: String,
}

/// What the gate decided once standing consent was applied.
#[derive(Debug, Clone)]
pub struct ApprovalCheck {
    pub result: ApprovalResult,
    /// Every prompt that was removed, and why. Empty whenever nothing was
    /// waived — including when the posture is `Off`, which resolves nothing.
    pub waived: Vec<WaivedApproval>,
    /// One line per prompt that was still asked about while a consent was
    /// consulted, naming the reason it did not cover the act. Empty when no
    /// consent was supplied at all, because nothing was consulted.
    pub asked: Vec<String>,
}

impl ApprovalCheck {
    /// Whether the caller must still pause.
    pub fn needs_approval(&self) -> bool {
        matches!(self.result, ApprovalResult::NeedsApproval { .. })
    }
}

impl ApprovalGate {
    /// [`Self::check`], with any standing consent the caller holds applied.
    ///
    /// A pending approval covered by a live envelope is dropped from the result
    /// **and debited against that envelope** — the debit is the authorisation,
    /// and one taken without a debit spends nothing, so the cap the owner
    /// granted would never be reached.
    ///
    /// Three properties hold whatever the caller passes:
    ///
    /// - **`None` waives nothing.** Identical to [`Self::check`].
    /// - **A commitment is never waived.** The class is computed by
    ///   `consequence_class_for_approval_rule`, which fails closed to
    ///   `commitment_or_transaction` for anything unclassified, and the resolver
    ///   refuses commitment for every envelope kind.
    /// - **A step the plan does not contain is never waived.** The pending
    ///   action names a step id; if that id is not in the plan handed in, the act
    ///   cannot be read and unreadable is not permission.
    pub fn check_against(
        &self,
        plan: &ExecutionPlan,
        constraints: &AgentConstraints,
        consent: Option<StandingConsent<'_>>,
    ) -> ApprovalCheck {
        // Destructured through a `match` rather than a `let`-`else`: the else
        // arm has to hand the untouched result back, and a `let`-`else` has
        // already moved it out of reach by then.
        let (checked_plan, pending_actions) = match self.check(plan, constraints) {
            ApprovalResult::NeedsApproval {
                plan: checked_plan,
                pending_actions,
            } => (checked_plan, pending_actions),
            approved => {
                return ApprovalCheck {
                    result: approved,
                    waived: Vec::new(),
                    asked: Vec::new(),
                }
            },
        };

        let Some(consent) = consent else {
            return ApprovalCheck {
                result: ApprovalResult::NeedsApproval {
                    plan: checked_plan,
                    pending_actions,
                },
                waived: Vec::new(),
                asked: Vec::new(),
            };
        };

        let mut still_pending: Vec<PendingApproval> = Vec::new();
        let mut waived: Vec<WaivedApproval> = Vec::new();
        let mut asked: Vec<String> = Vec::new();

        for pending in pending_actions {
            let Some(step) = plan.steps.iter().find(|step| step.id == pending.step_id) else {
                asked.push(format!(
                    "asked (no step `{}` in this plan)",
                    pending.step_id
                ));
                still_pending.push(pending);
                continue;
            };

            match resolve_step_waiver(&consent, step) {
                Ok(StepWaiver::Waived {
                    envelope_id,
                    outcome,
                    matched_predicates,
                    audit_line,
                }) => waived.push(WaivedApproval {
                    step_id: pending.step_id,
                    envelope_id,
                    outcome,
                    matched_predicates,
                    audit_line,
                }),
                Ok(StepWaiver::Ask { audit_line }) => {
                    asked.push(audit_line);
                    still_pending.push(pending);
                },
                Err(error) => {
                    // A resolution that failed to read must never read as
                    // coverage. The prompt stands.
                    asked.push(format!("asked (envelope resolution failed: {error:#})"));
                    still_pending.push(pending);
                },
            }
        }

        let result = if still_pending.is_empty() {
            ApprovalResult::Approved(checked_plan)
        } else {
            ApprovalResult::NeedsApproval {
                plan: checked_plan,
                pending_actions: still_pending,
            }
        };
        ApprovalCheck {
            result,
            waived,
            asked,
        }
    }
}

/// The flattened answer for one step, so the borrow of the envelope module's
/// enum does not escape this file.
enum StepWaiver {
    Ask {
        audit_line: String,
    },
    Waived {
        envelope_id: String,
        outcome: String,
        matched_predicates: Vec<String>,
        audit_line: String,
    },
}

/// Resolve one step against the standing consent.
///
/// The capability, the action and the effective request are all read from the
/// step. A caller cannot supply a softer capability than the one its own rule
/// matched, and the consequence class is derived inside
/// `resolve_approval_waiver` from that pair.
fn resolve_step_waiver(
    consent: &StandingConsent<'_>,
    step: &ExecutionStep,
) -> anyhow::Result<StepWaiver> {
    use crate::magician_v2::approval_envelopes::{
        preview_approval_waiver, resolve_approval_waiver, ApprovalContext,
    };

    let capability = step.tool.as_deref().unwrap_or_default();
    let action = step.action_type.as_deref().unwrap_or_default();
    let effective = crate::magician_v2::execution::effective_action::resolve_effective_action(
        capability,
        action,
        &step.params,
    );
    let act_ref = act_ref_for_step(consent.act_scope, step);

    let ctx = ApprovalContext {
        act_ref: &act_ref,
        effective: &effective,
        envelope_scope: consent.envelope_scope.cloned(),
        engagement_id: consent.engagement_id,
        engagement_identities: consent.engagement_identities,
        attachments_outside_ledger: consent.attachments_outside_ledger,
        value_micros: consent.value_micros,
        now: consent.now,
    };

    let resolved = if consent.preview_only {
        preview_approval_waiver(consent.gate, consent.mode, capability, action, ctx)?
    } else {
        resolve_approval_waiver(consent.gate, consent.mode, capability, action, ctx)?
    };

    let audit_line = resolved.audit_line();
    Ok(match resolved {
        crate::magician_v2::approval_envelopes::ApprovalWaiver::Waived {
            envelope_id,
            outcome,
            matched_predicates,
        } => StepWaiver::Waived {
            envelope_id,
            outcome,
            matched_predicates,
            audit_line,
        },
        crate::magician_v2::approval_envelopes::ApprovalWaiver::Ask { .. } => {
            StepWaiver::Ask { audit_line }
        },
    })
}

/// The key one act is debited under.
///
/// Derived from what the act **does** — the scope it belongs to, the capability,
/// the action and the arguments — and from nothing about when it was attempted.
/// That is the property that makes a retry resume rather than double-debit, and
/// it is why neither `step.id` nor `step.goal` is hashed: the id carries the
/// iteration number, so the same act retried on a later iteration would spend a
/// second act of headroom, and the goal is the free text the model wrote to
/// explain itself, which differs between two attempts at one act.
///
/// The parameters are ordered before hashing. A `HashMap` iterates in an order
/// that varies between processes, so hashing it directly would give one act a
/// different key on every run — every dispatch a fresh debit slot, and the cap
/// never reached.
///
/// An empty scope yields an empty key, which the envelope gate refuses as
/// `unidentified_act` rather than treating as a shared slot.
fn act_ref_for_step(act_scope: Option<&str>, step: &ExecutionStep) -> String {
    let Some(scope) = act_scope.map(str::trim).filter(|scope| !scope.is_empty()) else {
        return String::new();
    };

    let ordered: std::collections::BTreeMap<&str, &Value> = step
        .params
        .iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect();
    let payload = serde_json::to_string(&ordered).unwrap_or_default();

    let mut hasher = Sha256::new();
    for component in [
        scope,
        step.tool.as_deref().unwrap_or_default(),
        step.action_type.as_deref().unwrap_or_default(),
        payload.as_str(),
    ] {
        hasher.update(component.as_bytes());
        hasher.update([0x1fu8]);
    }
    format!("approval-act-{:x}", hasher.finalize())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::types::{ActionPattern, AgentConstraints, ApprovalRule};
    use serde_json::json;

    #[test]
    fn check_flags_matching_steps_for_approval() {
        let gate = ApprovalGate;
        let plan = ExecutionPlan {
            plan_id: "p1".into(),
            steps: vec![ExecutionStep {
                id: "s1".into(),
                goal: "submit application".into(),
                tool: Some("browser".into()),
                action_type: Some("submit".into()),
                params: HashMap::new(),
            }],
        };
        let constraints = AgentConstraints {
            requires_approval: vec![ApprovalRule {
                tool: "browser".into(),
                action: ActionPattern::Single("submit".into()),
                when: None,
                ttl_secs: None,
            }],
            ..AgentConstraints::default()
        };

        let result = gate.check(&plan, &constraints);
        assert!(matches!(result, ApprovalResult::NeedsApproval { .. }));
    }

    #[test]
    fn condition_match_checks_url_and_params() {
        let rule = ApprovalRule {
            tool: "browser".into(),
            action: ActionPattern::Single("navigate".into()),
            when: Some(ApprovalCondition {
                param_matches: HashMap::from([("selector".to_string(), vec!["apply".to_string()])]),
                url_contains: vec!["linkedin.com".to_string()],
            }),
            ttl_secs: None,
        };
        let step = ExecutionStep {
            id: "s1".into(),
            goal: "navigate".into(),
            tool: Some("browser".into()),
            action_type: Some("navigate".into()),
            params: HashMap::from([
                ("selector".to_string(), json!("#apply-now")),
                ("url".to_string(), json!("https://linkedin.com/jobs")),
            ]),
        };
        assert!(rule_matches(&rule, &step));
    }

    #[test]
    fn content_approval_conditions_check_each_vector_branch() {
        let rule = ApprovalRule {
            tool: "content_read".into(),
            action: ActionPattern::Single("*".into()),
            when: Some(ApprovalCondition {
                param_matches: HashMap::new(),
                url_contains: vec!["private.example.test".into()],
            }),
            ttl_secs: None,
        };
        let step = ExecutionStep {
            id: "vector-read".into(),
            goal: "compare reports".into(),
            tool: Some("content_read".into()),
            action_type: Some("*".into()),
            params: HashMap::from([(
                "requests".into(),
                json!([
                    {"url": "https://public.example.test/report"},
                    {"url": "https://private.example.test/report"}
                ]),
            )]),
        };
        assert!(rule_matches(&rule, &step));

        let query_rule = ApprovalRule {
            tool: "content_search".into(),
            action: ActionPattern::Single("*".into()),
            when: Some(ApprovalCondition {
                param_matches: HashMap::from([("query".into(), vec!["confidential".into()])]),
                url_contains: Vec::new(),
            }),
            ttl_secs: None,
        };
        let query_step = ExecutionStep {
            id: "vector-search".into(),
            goal: "search".into(),
            tool: Some("content_search".into()),
            action_type: Some("*".into()),
            params: HashMap::from([(
                "requests".into(),
                json!([{"query": "public"}, {"query": "confidential roadmap"}]),
            )]),
        };
        assert!(rule_matches(&query_rule, &query_step));
    }

    #[test]
    fn condition_match_is_or_when_both_groups_defined_param_only_match() {
        let rule = ApprovalRule {
            tool: "browser".into(),
            action: ActionPattern::Single("navigate".into()),
            when: Some(ApprovalCondition {
                param_matches: HashMap::from([("selector".to_string(), vec!["apply".to_string()])]),
                url_contains: vec!["example.com/secure".to_string()],
            }),
            ttl_secs: None,
        };
        let step = ExecutionStep {
            id: "s1".into(),
            goal: "navigate".into(),
            tool: Some("browser".into()),
            action_type: Some("navigate".into()),
            params: HashMap::from([
                ("selector".to_string(), json!("#apply-now")),
                ("url".to_string(), json!("https://example.com/public")),
            ]),
        };
        assert!(rule_matches(&rule, &step));
    }

    #[test]
    fn condition_match_is_or_when_both_groups_defined_url_only_match() {
        let rule = ApprovalRule {
            tool: "browser".into(),
            action: ActionPattern::Single("navigate".into()),
            when: Some(ApprovalCondition {
                param_matches: HashMap::from([("selector".to_string(), vec!["apply".to_string()])]),
                url_contains: vec!["linkedin.com".to_string()],
            }),
            ttl_secs: None,
        };
        let step = ExecutionStep {
            id: "s1".into(),
            goal: "navigate".into(),
            tool: Some("browser".into()),
            action_type: Some("navigate".into()),
            params: HashMap::from([
                ("selector".to_string(), json!("#not-a-match")),
                ("url".to_string(), json!("https://linkedin.com/jobs")),
            ]),
        };
        assert!(rule_matches(&rule, &step));
    }

    #[test]
    fn non_matching_step_is_auto_approved() {
        let gate = ApprovalGate;
        let plan = ExecutionPlan {
            plan_id: "p1".into(),
            steps: vec![ExecutionStep {
                id: "s1".into(),
                goal: "open dashboard".into(),
                tool: Some("browser".into()),
                action_type: Some("navigate".into()),
                params: HashMap::new(),
            }],
        };
        let constraints = AgentConstraints {
            requires_approval: vec![ApprovalRule {
                tool: "browser".into(),
                action: ActionPattern::Single("submit".into()),
                when: None,
                ttl_secs: None,
            }],
            ..AgentConstraints::default()
        };

        let result = gate.check(&plan, &constraints);
        assert!(matches!(result, ApprovalResult::Approved(_)));
    }

    #[test]
    fn wildcard_action_pattern_matches_any_action() {
        let rule = ApprovalRule {
            tool: "browser".into(),
            action: ActionPattern::Single("*".into()),
            when: None,
            ttl_secs: None,
        };
        let step = ExecutionStep {
            id: "s1".into(),
            goal: "click button".into(),
            tool: Some("browser".into()),
            action_type: Some("click".into()),
            params: HashMap::new(),
        };

        assert!(rule_matches(&rule, &step));
    }

    #[test]
    fn multiple_action_pattern_matches_list_member() {
        let rule = ApprovalRule {
            tool: "browser".into(),
            action: ActionPattern::Multiple(vec!["type".into(), "submit".into()]),
            when: None,
            ttl_secs: None,
        };
        let step = ExecutionStep {
            id: "s1".into(),
            goal: "type in form".into(),
            tool: Some("browser".into()),
            action_type: Some("type".into()),
            params: HashMap::new(),
        };

        assert!(rule_matches(&rule, &step));
    }

    #[test]
    fn when_clause_with_empty_conditions_is_vacuously_true() {
        let rule = ApprovalRule {
            tool: "browser".into(),
            action: ActionPattern::Single("navigate".into()),
            when: Some(ApprovalCondition {
                param_matches: HashMap::new(),
                url_contains: Vec::new(),
            }),
            ttl_secs: None,
        };
        let step = ExecutionStep {
            id: "s1".into(),
            goal: "navigate".into(),
            tool: Some("browser".into()),
            action_type: Some("navigate".into()),
            params: HashMap::new(),
        };

        assert!(rule_matches(&rule, &step));
    }

    #[test]
    fn when_clause_with_non_empty_conditions_and_no_match_is_false() {
        let rule = ApprovalRule {
            tool: "browser".into(),
            action: ActionPattern::Single("navigate".into()),
            when: Some(ApprovalCondition {
                param_matches: HashMap::from([("selector".to_string(), vec!["apply".to_string()])]),
                url_contains: vec!["linkedin.com".to_string()],
            }),
            ttl_secs: None,
        };
        let step = ExecutionStep {
            id: "s1".into(),
            goal: "navigate".into(),
            tool: Some("browser".into()),
            action_type: Some("navigate".into()),
            params: HashMap::from([
                ("selector".to_string(), json!("#home-link")),
                ("url".to_string(), json!("https://example.com/home")),
            ]),
        };

        assert!(!rule_matches(&rule, &step));
    }

    #[test]
    fn when_clause_param_match_result_is_stable_across_map_insertion_order() {
        let mut first = HashMap::new();
        first.insert("alpha".to_string(), vec!["nope".to_string()]);
        first.insert("beta".to_string(), vec!["needle".to_string()]);

        let mut second = HashMap::new();
        second.insert("beta".to_string(), vec!["needle".to_string()]);
        second.insert("alpha".to_string(), vec!["nope".to_string()]);

        let step = ExecutionStep {
            id: "s1".into(),
            goal: "navigate".into(),
            tool: Some("browser".into()),
            action_type: Some("navigate".into()),
            params: HashMap::from([("beta".to_string(), json!("contains needle"))]),
        };

        let first_result = condition_matches(
            &ApprovalCondition {
                param_matches: first,
                url_contains: Vec::new(),
            },
            &step,
        );
        let second_result = condition_matches(
            &ApprovalCondition {
                param_matches: second,
                url_contains: Vec::new(),
            },
            &step,
        );

        assert!(first_result);
        assert_eq!(first_result, second_result);
    }

    #[test]
    fn when_clause_matches_numeric_param_values() {
        let rule = ApprovalRule {
            tool: "http".into(),
            action: ActionPattern::Single("post".into()),
            when: Some(ApprovalCondition {
                param_matches: HashMap::from([("status".to_string(), vec!["201".to_string()])]),
                url_contains: Vec::new(),
            }),
            ttl_secs: None,
        };
        let step = ExecutionStep {
            id: "s1".into(),
            goal: "post data".into(),
            tool: Some("http".into()),
            action_type: Some("post".into()),
            params: HashMap::from([("status".to_string(), json!(201))]),
        };

        assert!(rule_matches(&rule, &step));
    }

    #[test]
    fn pack_tool_derivation_handles_flat_and_namespaced_names() {
        // Flat compiled tool name (no `__`) → the name itself.
        assert_eq!(pack_tool_for_approval("imessage_send"), "imessage_send");
        // Namespaced tool → pack prefix.
        assert_eq!(pack_tool_for_approval("zepto-mcp__call_tool"), "zepto-mcp");
    }

    #[test]
    fn agent_requires_approval_gates_flat_compiled_tool() {
        // A rule on a flat compiled tool (`imessage_send`) with a wildcard
        // action — mirrors how the chat fork decides to route the tool
        // through the executor. The flat name has no `__`, so the action
        // token falls back to `execute`; the wildcard matches it.
        let constraints = AgentConstraints {
            requires_approval: vec![ApprovalRule {
                tool: "imessage_send".into(),
                action: crate::magician_v2::agents::types::ActionPattern::Single("*".into()),
                when: None,
                ttl_secs: None,
            }],
            ..AgentConstraints::default()
        };
        assert!(agent_requires_approval_for_pack_tool(
            &constraints,
            "imessage_send",
            &HashMap::new()
        ));
        // A different tool with the same rule set must NOT be gated.
        assert!(!agent_requires_approval_for_pack_tool(
            &constraints,
            "search_memory",
            &HashMap::new()
        ));
    }

    #[test]
    fn harness_mutation_rules_gate_each_mutation_tool_not_work_assignment() {
        // The central harness mutation gate must pause each of the 4
        // consequential mutation tools (empty params → action falls back to
        // `execute`, matched by the `"*"` wildcard) and must NOT gate normal
        // work assignment (`create_task`) or the ungated backlog tools.
        let constraints = AgentConstraints {
            requires_approval: harness_mutation_approval_rules(),
            ..AgentConstraints::default()
        };
        for tool in crate::magician_v2::harness::HARNESS_APPROVAL_GATED_TOOL_NAMES {
            assert!(
                agent_requires_approval_for_pack_tool(&constraints, tool, &HashMap::new()),
                "harness mutation tool {tool} must require owner approval"
            );
        }
        assert!(!agent_requires_approval_for_pack_tool(
            &constraints,
            "create_task",
            &HashMap::new()
        ));
        assert!(!agent_requires_approval_for_pack_tool(
            &constraints,
            "propose_backlog_item",
            &HashMap::new()
        ));
    }

    #[test]
    fn agent_requires_approval_is_false_without_rules() {
        let constraints = AgentConstraints::default();
        assert!(!agent_requires_approval_for_pack_tool(
            &constraints,
            "imessage_send",
            &HashMap::new()
        ));
    }

    #[test]
    fn agent_requires_approval_respects_action_discriminator() {
        // A rule gated on a specific action only matches when the resolved
        // params carry that action discriminator — proving the chat fork's
        // decision is action-aware and identical to the executor's
        // `pack_action_for_approval` derivation.
        let constraints = AgentConstraints {
            requires_approval: vec![ApprovalRule {
                tool: "files".into(),
                action: crate::magician_v2::agents::types::ActionPattern::Single("delete".into()),
                when: None,
                ttl_secs: None,
            }],
            ..AgentConstraints::default()
        };
        let mut delete_params = HashMap::new();
        delete_params.insert("action".to_string(), json!("delete"));
        assert!(agent_requires_approval_for_pack_tool(
            &constraints,
            "files",
            &delete_params
        ));
        let mut read_params = HashMap::new();
        read_params.insert("action".to_string(), json!("read"));
        assert!(!agent_requires_approval_for_pack_tool(
            &constraints,
            "files",
            &read_params
        ));
    }

    #[test]
    fn structural_controls_use_canonical_orchestrator_approval_coordinates() {
        for action in [
            "spawn_sub_goal",
            "delegate_to_agent",
            "handover_to_agent",
            "orchestrate_pipeline",
            "yield",
            "need_user_input",
            "delegate_to_chat",
        ] {
            assert_eq!(
                tool_action_for_approval(action, &HashMap::new()),
                ("orchestrator".to_string(), action.to_string())
            );
            let constraints = AgentConstraints {
                requires_approval: vec![ApprovalRule {
                    tool: "orchestrator".into(),
                    action: ActionPattern::Single(action.into()),
                    when: None,
                    ttl_secs: None,
                }],
                ..AgentConstraints::default()
            };
            assert!(agent_requires_approval_for_tool(
                &constraints,
                action,
                &HashMap::new()
            ));
            assert!(structural_action_may_require_approval(
                &constraints.requires_approval,
                action
            ));
        }
    }

    #[test]
    fn structural_approval_projection_requires_orchestrator_tool_match() {
        let flat_but_wrong = vec![ApprovalRule {
            tool: "delegate_to_agent".into(),
            action: ActionPattern::Single("execute".into()),
            when: None,
            ttl_secs: None,
        }];
        assert!(!structural_action_may_require_approval(
            &flat_but_wrong,
            "delegate_to_agent"
        ));
    }

    #[test]
    fn harness_merged_rules_gate_mutations_only_for_harness_agents() {
        use crate::magician_v2::agents::types::AgentDefinition;

        // A harness agent with no YAML `requires_approval` rules must still
        // have every consequential mutation tool gated by the central merge.
        let harness_def = AgentDefinition::from_yaml_str(
            r#"
agent_id: "cro"
name: "CRO"
persona: "Chief Revenue Officer"
kind: personal
harness: {}
"#,
        )
        .expect("harness definition");
        let harness_rules = harness_merged_approval_rules(&harness_def);
        for tool in crate::magician_v2::harness::HARNESS_APPROVAL_GATED_TOOL_NAMES {
            assert!(
                harness_rules.iter().any(|rule| rule.tool == *tool),
                "harness merge must gate mutation tool {tool}"
            );
        }
        // And the merged set actually pauses dispatch through the shared probe.
        let harness_constraints = AgentConstraints {
            requires_approval: harness_rules,
            ..AgentConstraints::default()
        };
        assert!(agent_requires_approval_for_pack_tool(
            &harness_constraints,
            "create_agent",
            &HashMap::new()
        ));

        // A non-harness agent gets NONE of the mutation gates injected.
        let plain_def = AgentDefinition::from_yaml_str(
            r#"
agent_id: "worker"
name: "Worker"
persona: "Just a worker"
"#,
        )
        .expect("plain definition");
        let plain_rules = harness_merged_approval_rules(&plain_def);
        for tool in crate::magician_v2::harness::HARNESS_APPROVAL_GATED_TOOL_NAMES {
            assert!(
                !plain_rules.iter().any(|rule| rule.tool == *tool),
                "non-harness agent must NOT gate mutation tool {tool}"
            );
        }
        assert!(plain_rules.is_empty());
    }

    #[test]
    fn when_clause_matches_boolean_param_values() {
        let rule = ApprovalRule {
            tool: "browser".into(),
            action: ActionPattern::Single("click".into()),
            when: Some(ApprovalCondition {
                param_matches: HashMap::from([("confirmed".to_string(), vec!["true".to_string()])]),
                url_contains: Vec::new(),
            }),
            ttl_secs: None,
        };
        let step = ExecutionStep {
            id: "s1".into(),
            goal: "confirm dialog".into(),
            tool: Some("browser".into()),
            action_type: Some("click".into()),
            params: HashMap::from([("confirmed".to_string(), json!(true))]),
        };

        assert!(rule_matches(&rule, &step));
    }

    // ── Standing consent in front of the rules ──────────────────────────

    mod standing_consent {
        use super::*;
        use crate::magician_v2::agents::ConsequenceClass;
        use crate::magician_v2::approval_envelopes::{
            ApprovalEnvelopeStore, EnvelopeGate, EnvelopeKind, EnvelopeLimits, EnvelopeMode,
            EnvelopeScope, EnvelopeStoreScope, GrantEnvelope,
        };
        use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
        use chrono::{Duration, TimeZone};

        const ENGAGEMENT: &str = "eng-1";
        const OUTCOME: &str = "correspond with the people this work is about";

        fn at() -> DateTime<Utc> {
            Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
        }

        fn workspace() -> (ApprovalEnvelopeStore, EnvelopeStoreScope, tempfile::TempDir) {
            let dir = tempfile::tempdir().expect("tempdir");
            (
                ApprovalEnvelopeStore::new(ArtifactV2Workspace::new(dir.path().to_path_buf())),
                EnvelopeStoreScope::new("anonymous", "default"),
                dir,
            )
        }

        /// A standing envelope for bounded communication, with room for `max_acts`.
        fn grant(
            store: &ApprovalEnvelopeStore,
            scope: &EnvelopeStoreScope,
            max_acts: u32,
        ) -> String {
            store
                .grant(
                    scope,
                    &GrantEnvelope {
                        scope: EnvelopeScope::Engagement(ENGAGEMENT.to_string()),
                        outcome: OUTCOME.to_string(),
                        kind: EnvelopeKind::Standing,
                        covers: vec![ConsequenceClass::BoundedCommunication],
                        limits: EnvelopeLimits {
                            expires_at: Some(at() + Duration::days(7)),
                            max_acts: Some(max_acts),
                            ..EnvelopeLimits::default()
                        },
                        boundary: Vec::new(),
                        granted_by: "owner".to_string(),
                    },
                    at() - Duration::hours(1),
                )
                .expect("grant")
                .envelope_id
        }

        /// One gated mail send, in the shape `approval_step_from_candidate` builds.
        fn send_step(id: &str, goal: &str) -> ExecutionStep {
            ExecutionStep {
                id: id.to_string(),
                goal: goal.to_string(),
                tool: Some("gmail".into()),
                action_type: Some("send".into()),
                params: HashMap::from([
                    ("to".to_string(), json!("someone@example.com")),
                    ("subject".to_string(), json!("hello")),
                    ("body".to_string(), json!("hi")),
                ]),
            }
        }

        fn plan_of(step: ExecutionStep) -> ExecutionPlan {
            ExecutionPlan {
                plan_id: "p1".into(),
                steps: vec![step],
            }
        }

        fn gated(tool: &str) -> AgentConstraints {
            AgentConstraints {
                requires_approval: vec![ApprovalRule {
                    tool: tool.into(),
                    action: ActionPattern::Single("*".into()),
                    when: None,
                    ttl_secs: None,
                }],
                ..AgentConstraints::default()
            }
        }

        fn consent<'a>(
            gate: &'a EnvelopeGate,
            mode: EnvelopeMode,
            scope: &'a EnvelopeScope,
            identities: &'a [String],
            preview_only: bool,
        ) -> StandingConsent<'a> {
            StandingConsent {
                gate,
                mode,
                act_scope: Some("exec-1"),
                envelope_scope: Some(scope),
                engagement_id: Some(ENGAGEMENT),
                engagement_identities: identities,
                attachments_outside_ledger: Some(0),
                value_micros: None,
                now: at(),
                preview_only,
            }
        }

        /// The default posture must waive nothing.
        ///
        /// Pins the plan's first acceptance criterion at this layer: with the
        /// mode `Off`, an approval that would be asked is still asked, no
        /// envelope is consulted, and the ledger is untouched. A regression here
        /// would silence prompts in every process that never wired a posture —
        /// which is every process that forgot to.
        #[test]
        fn the_off_posture_waives_nothing_and_debits_nothing() {
            let (store, store_scope, _dir) = workspace();
            let envelope_id = grant(&store, &store_scope, 10);
            let gate = EnvelopeGate::new(store.clone(), store_scope.clone());
            let scope = EnvelopeScope::Engagement(ENGAGEMENT.to_string());
            let identities: Vec<String> = Vec::new();

            let plan = plan_of(send_step("iter-1-candidate-0", "mail the counterparty"));
            let checked = ApprovalGate.check_against(
                &plan,
                &gated("gmail"),
                Some(consent(
                    &gate,
                    EnvelopeMode::Off,
                    &scope,
                    &identities,
                    false,
                )),
            );

            assert!(checked.waived.is_empty());
            assert_eq!(checked.asked, vec!["asked (envelopes off)".to_string()]);
            match checked.result {
                ApprovalResult::NeedsApproval {
                    ref pending_actions,
                    ..
                } => {
                    assert_eq!(pending_actions.len(), 1);
                    assert_eq!(pending_actions[0].step_id, "iter-1-candidate-0");
                },
                ApprovalResult::Approved(_) => panic!("`off` must never waive an approval"),
            }

            let state = store
                .load(&store_scope, &envelope_id)
                .expect("load")
                .expect("envelope");
            assert_eq!(state.acts_used(), 0);
        }

        /// A covered act under the enforcing posture loses its prompt AND is
        /// debited.
        ///
        /// The debit is the point: a waiver taken without one spends nothing, so
        /// a ten-act envelope would authorise an unbounded number of waivers and
        /// the cap the owner set would never be reached.
        #[test]
        fn an_enforcing_envelope_waives_the_prompt_and_records_the_consumption() {
            let (store, store_scope, _dir) = workspace();
            let envelope_id = grant(&store, &store_scope, 10);
            let gate = EnvelopeGate::new(store.clone(), store_scope.clone());
            let scope = EnvelopeScope::Engagement(ENGAGEMENT.to_string());
            let identities: Vec<String> = Vec::new();

            let plan = plan_of(send_step("iter-1-candidate-0", "mail the counterparty"));
            let checked = ApprovalGate.check_against(
                &plan,
                &gated("gmail"),
                Some(consent(
                    &gate,
                    EnvelopeMode::Enforcing,
                    &scope,
                    &identities,
                    false,
                )),
            );

            assert!(
                matches!(checked.result, ApprovalResult::Approved(_)),
                "a covered act must not pause"
            );
            assert_eq!(checked.waived.len(), 1);
            assert_eq!(checked.waived[0].step_id, "iter-1-candidate-0");
            assert_eq!(checked.waived[0].envelope_id, envelope_id);
            assert_eq!(checked.waived[0].outcome, OUTCOME);
            assert!(checked.asked.is_empty());

            let state = store
                .load(&store_scope, &envelope_id)
                .expect("load")
                .expect("envelope");
            assert_eq!(state.acts_used(), 1);
            assert_eq!(state.consumed[0].recipients, vec!["someone@example.com"]);
        }

        /// The same act checked twice resumes one debit rather than spending two.
        ///
        /// The act key is derived from what the act does, so a retry — a resumed
        /// execution re-reaching the same gate — must land in the slot its first
        /// attempt already filled. Hashing the step id instead would spend a
        /// second act, because the id carries the iteration number.
        #[test]
        fn an_identical_replay_resumes_one_debit() {
            let (store, store_scope, _dir) = workspace();
            let envelope_id = grant(&store, &store_scope, 10);
            let gate = EnvelopeGate::new(store.clone(), store_scope.clone());
            let scope = EnvelopeScope::Engagement(ENGAGEMENT.to_string());
            let identities: Vec<String> = Vec::new();

            for (id, goal) in [
                ("iter-1-candidate-0", "mail the counterparty"),
                ("iter-4-candidate-0", "resumed after the pause"),
            ] {
                let plan = plan_of(send_step(id, goal));
                let checked = ApprovalGate.check_against(
                    &plan,
                    &gated("gmail"),
                    Some(consent(
                        &gate,
                        EnvelopeMode::Enforcing,
                        &scope,
                        &identities,
                        false,
                    )),
                );
                assert_eq!(checked.waived.len(), 1, "{id} must waive");
                assert_eq!(checked.waived[0].envelope_id, envelope_id);
            }

            let state = store
                .load(&store_scope, &envelope_id)
                .expect("load")
                .expect("envelope");
            assert_eq!(
                state.acts_used(),
                1,
                "the iteration number and the model's reasoning must not change the act key"
            );
        }

        /// An act nobody classified is a commitment, and a commitment is never
        /// waived.
        ///
        /// `some-unclassified-tool` is not outward and not a known commitment, so
        /// the ordinary classifier calls it `private_local`. The approval-rule
        /// classifier refuses that answer for an act somebody deliberately gated
        /// and fails closed to commitment. Softening this would delete a gate the
        /// owner believes is there.
        #[test]
        fn an_unclassified_gated_act_is_never_waived() {
            let (store, store_scope, _dir) = workspace();
            let envelope_id = grant(&store, &store_scope, 10);
            let gate = EnvelopeGate::new(store.clone(), store_scope.clone());
            let scope = EnvelopeScope::Engagement(ENGAGEMENT.to_string());
            let identities: Vec<String> = Vec::new();

            let mut step = send_step("iter-1-candidate-0", "do the unclassified thing");
            step.tool = Some("some-unclassified-tool".into());
            step.action_type = Some("do_it".into());

            let checked = ApprovalGate.check_against(
                &plan_of(step),
                &gated("some-unclassified-tool"),
                Some(consent(
                    &gate,
                    EnvelopeMode::Enforcing,
                    &scope,
                    &identities,
                    false,
                )),
            );

            assert!(checked.waived.is_empty());
            assert!(matches!(
                checked.result,
                ApprovalResult::NeedsApproval { .. }
            ));
            assert_eq!(
                checked.asked,
                vec!["asked (class_not_coverable[commitment_or_transaction/any])".to_string()]
            );

            let state = store
                .load(&store_scope, &envelope_id)
                .expect("load")
                .expect("envelope");
            assert_eq!(state.acts_used(), 0);
        }

        /// Shadow observes and still asks, and it spends nothing while doing so.
        ///
        /// A shadow that debited would describe a history that never happened —
        /// caps spent on acts the owner approved by hand — and the whole point of
        /// the posture is to compare the resolver against reality.
        #[test]
        fn shadow_still_asks_and_spends_nothing() {
            let (store, store_scope, _dir) = workspace();
            let envelope_id = grant(&store, &store_scope, 10);
            let gate = EnvelopeGate::new(store.clone(), store_scope.clone());
            let scope = EnvelopeScope::Engagement(ENGAGEMENT.to_string());
            let identities: Vec<String> = Vec::new();

            let checked = ApprovalGate.check_against(
                &plan_of(send_step("iter-1-candidate-0", "mail the counterparty")),
                &gated("gmail"),
                Some(consent(
                    &gate,
                    EnvelopeMode::Shadow,
                    &scope,
                    &identities,
                    false,
                )),
            );

            assert!(checked.waived.is_empty());
            assert!(matches!(
                checked.result,
                ApprovalResult::NeedsApproval { .. }
            ));
            assert_eq!(checked.asked, vec!["asked (not_enforcing)".to_string()]);

            let state = store
                .load(&store_scope, &envelope_id)
                .expect("load")
                .expect("envelope");
            assert_eq!(state.acts_used(), 0);
        }

        /// A preview never authorises and never debits, whatever the posture.
        ///
        /// It exists so a surface can render "this will pause" without spending
        /// an act per render. A preview that could waive would let a screen
        /// consume the owner's envelope by being looked at.
        #[test]
        fn a_preview_never_waives_and_never_debits() {
            let (store, store_scope, _dir) = workspace();
            let envelope_id = grant(&store, &store_scope, 10);
            let gate = EnvelopeGate::new(store.clone(), store_scope.clone());
            let scope = EnvelopeScope::Engagement(ENGAGEMENT.to_string());
            let identities: Vec<String> = Vec::new();

            let checked = ApprovalGate.check_against(
                &plan_of(send_step("iter-1-candidate-0", "mail the counterparty")),
                &gated("gmail"),
                Some(consent(
                    &gate,
                    EnvelopeMode::Enforcing,
                    &scope,
                    &identities,
                    true,
                )),
            );

            assert!(checked.waived.is_empty());
            assert_eq!(checked.asked, vec!["asked (not_enforcing)".to_string()]);
            let state = store
                .load(&store_scope, &envelope_id)
                .expect("load")
                .expect("envelope");
            assert_eq!(state.acts_used(), 0);
        }

        /// Supplying no consent is exactly today's behaviour.
        ///
        /// The whole layer has to be inert for every caller that has not opted
        /// in, or wiring one gate would change the others.
        #[test]
        fn without_consent_the_check_is_unchanged() {
            let plan = plan_of(send_step("iter-1-candidate-0", "mail the counterparty"));
            let constraints = gated("gmail");

            let bare = ApprovalGate.check(&plan, &constraints);
            let checked = ApprovalGate.check_against(&plan, &constraints, None);

            assert!(checked.waived.is_empty());
            assert!(checked.asked.is_empty());
            match (bare, checked.result) {
                (
                    ApprovalResult::NeedsApproval {
                        pending_actions: expected,
                        ..
                    },
                    ApprovalResult::NeedsApproval {
                        pending_actions: actual,
                        ..
                    },
                ) => {
                    assert_eq!(actual.len(), expected.len());
                    assert_eq!(actual[0].step_id, expected[0].step_id);
                    assert_eq!(actual[0].matched_rule.tool, expected[0].matched_rule.tool);
                },
                _ => panic!("a gated act must still need approval when no consent is supplied"),
            }
        }

        /// An act with no stable scope key cannot be debited, so it is asked
        /// about rather than waived.
        ///
        /// Without this every keyless act would share one debit slot: the first
        /// would record a debit and each later one would read as a replay of it,
        /// so an envelope for ten would authorise an unbounded number.
        #[test]
        fn an_act_with_no_scope_key_is_asked_about() {
            let (store, store_scope, _dir) = workspace();
            let envelope_id = grant(&store, &store_scope, 10);
            let gate = EnvelopeGate::new(store.clone(), store_scope.clone());
            let scope = EnvelopeScope::Engagement(ENGAGEMENT.to_string());
            let identities: Vec<String> = Vec::new();

            let mut unkeyed = consent(&gate, EnvelopeMode::Enforcing, &scope, &identities, false);
            unkeyed.act_scope = Some("   ");

            let checked = ApprovalGate.check_against(
                &plan_of(send_step("iter-1-candidate-0", "mail the counterparty")),
                &gated("gmail"),
                Some(unkeyed),
            );

            assert!(checked.waived.is_empty());
            assert_eq!(checked.asked, vec!["asked (unidentified_act)".to_string()]);
            let state = store
                .load(&store_scope, &envelope_id)
                .expect("load")
                .expect("envelope");
            assert_eq!(state.acts_used(), 0);
        }

        /// An act carrying an argument escape hatch is never waived.
        ///
        /// The tokens nobody inspected can still change what the act does, so a
        /// waiver would authorise a description of the act rather than the act.
        /// This is the property that makes envelopes worth having in front of a
        /// raw sending skill at all.
        #[test]
        fn an_unbindable_act_is_never_waived() {
            let (store, store_scope, _dir) = workspace();
            let envelope_id = grant(&store, &store_scope, 10);
            let gate = EnvelopeGate::new(store.clone(), store_scope.clone());
            let scope = EnvelopeScope::Engagement(ENGAGEMENT.to_string());
            let identities: Vec<String> = Vec::new();

            let mut step = send_step("iter-1-candidate-0", "mail the counterparty");
            step.params.insert(
                "extra_args".to_string(),
                json!(["--bcc", "quiet@example.com"]),
            );

            let checked = ApprovalGate.check_against(
                &plan_of(step),
                &gated("gmail"),
                Some(consent(
                    &gate,
                    EnvelopeMode::Enforcing,
                    &scope,
                    &identities,
                    false,
                )),
            );

            assert!(checked.waived.is_empty());
            assert_eq!(
                checked.asked,
                vec!["asked (action_not_bindable[extra_args])".to_string()]
            );
            let state = store
                .load(&store_scope, &envelope_id)
                .expect("load")
                .expect("envelope");
            assert_eq!(state.acts_used(), 0);
        }

        /// The act key ignores the iteration and the model's reasoning, and
        /// changes when the arguments do.
        ///
        /// Both halves matter. If the key moved with the iteration, a retry would
        /// double-debit; if it stayed put when the recipient changed, a second
        /// send to somebody else would ride the first one's debit.
        #[test]
        fn the_act_key_follows_the_arguments_and_nothing_else() {
            let first = send_step("iter-1-candidate-0", "mail the counterparty");
            let mut same_act_later = send_step("iter-9-candidate-0", "trying again after a pause");
            // Re-inserted in a different order to prove the key does not depend
            // on `HashMap` iteration order.
            same_act_later.params = HashMap::from([
                ("body".to_string(), json!("hi")),
                ("subject".to_string(), json!("hello")),
                ("to".to_string(), json!("someone@example.com")),
            ]);
            let mut different_recipient = send_step("iter-1-candidate-0", "mail the counterparty");
            different_recipient
                .params
                .insert("to".to_string(), json!("someone-else@example.com"));

            let key = |step: &ExecutionStep| act_ref_for_step(Some("exec-1"), step);
            assert_eq!(key(&first), key(&same_act_later));
            assert_ne!(key(&first), key(&different_recipient));
            assert_ne!(key(&first), act_ref_for_step(Some("exec-2"), &first));
            assert_eq!(act_ref_for_step(None, &first), String::new());
            assert_eq!(act_ref_for_step(Some(""), &first), String::new());
        }
    }
}
