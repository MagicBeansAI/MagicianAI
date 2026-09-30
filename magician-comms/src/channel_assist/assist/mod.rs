//! The channel-assist assist seam (plan workstream 3.1,
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! Channel Assist's PRODUCT lane lives here as a seam-registered module
//! (the plan's default form — not an app package: no stated packaging
//! benefit, and packaging would imply a UI rewrite). One module tree owns
//! every product decision on top of the comms substrate:
//!
//! - [`classify`] — the classification policy: the four-label vocabulary,
//!   off-vocabulary coercion, confidence clamping, actionable-label
//!   promotion, and the annotation state a routed classification lands in.
//! - [`reconcile`] — the reconciliation rules (retire / supersede /
//!   require-review / stale / routing-retracted) and the repair sweep.
//! - [`distill`] — the local-only distillation policy: the information-brief
//!   and follow-up-hint contracts, response schemas, and validation.
//! - [`content`] — the pure in-memory content prep the distiller reads
//!   (MIME extraction, quote stripping, bounded chunking). Nothing here is
//!   persisted.
//! - [`reply_draft`] — the local-pinned reply drafting policy.
//! - [`writing_preferences`] — the per-sender/domain writing-preference
//!   policy and its memory-tier promotion.
//! - [`fixtures`] / [`draft_eval`] / [`classify`] (its `EvalFixture` set) —
//!   the export fixture contract and the classifier/draft evals.
//! - [`quality_budgets`] — the product-lane latency budgets (Phase 8's
//!   Today-projection budget, moved from the API handler file).
//! - [`completion_port`] — plan 3.1 prerequisite (a): the port through which
//!   reconciliation reports owner-proved completions to attention learning
//!   (lib-side since 3.0) without naming it.
//!
//! What did NOT move (Layer 1 substrate, unchanged locations): the
//! connectors/adapters (`adapters/`, `adapter_registry`), the ingestors
//! (`ingest*`), `gws_client`, `registry`, `sync`, `store` (the scoped
//! DuckDB plane — including the annotation-lifecycle application functions,
//! which are transaction/SQL-bound; the DECISIONS they apply live here),
//! `channel_providers` (the transport registry), `live_content`,
//! `channel_observe`, the memory/evidence/feedback/pattern bridges, and the
//! 3.0 comms-coupled worker trees (`resurfacing`, `attention_learning` —
//! their engine glob re-export shims were removed by Phase 5, batch 5 of
//! the 2026-08-28 removal inventory; the engine types import from
//! `magician_v2::attention::` directly).
//!
//! Compat shims removed (Phase 5, batch 4 of the 2026-08-28 removal
//! inventory): the pre-3.1 flat modules
//! `channel_assist::{classify, content, distill, draft_eval, fixtures,
//! reconcile, reply_draft, writing_preferences}` were `pub use` re-export
//! shims over this tree; every consumer —
//! `magician-api/src/channel_assist_api.rs` handlers, `magician-bin`
//! worker wiring, the ingestors and adapters, the memory bridges, and
//! `magician/tests/phase0_wire_oracles.rs` — now imports through
//! [`assist`] directly. The moved files themselves are unedited except
//! where a 3.1 port required it (the completion-port extraction in
//! `reconcile`, and policy-surface visibility widened for the seam's
//! tests); their pre-existing test suites moved with them verbatim.
//!
//! The substrate re-exports below (`store`, `types`, …) exist so the moved
//! files' `super::` references — written when they lived one level up in
//! `channel_assist/` — keep resolving unchanged, including inside their
//! test modules (`super::super::types` from `assist/<file>`'s tests is
//! `assist::types`, which is the same module).

pub mod classify;
pub mod completion_port;
pub mod content;
pub mod distill;
pub mod draft_eval;
pub mod fixtures;
pub mod quality_budgets;
pub mod reconcile;
pub mod reply_draft;
pub mod writing_preferences;

// Substrate re-exports (see the module doc): the moved product modules
// reference these through `super::` exactly as they did above the seam, and
// the re-exports also keep their in-file tests' `super::super::…` paths
// resolving. These are aliases of the substrate modules — not copies.
pub use crate::channel_assist::adapter_registry;
pub use crate::channel_assist::channel_providers;
pub use crate::channel_assist::gws_client;
pub use crate::channel_assist::store;
pub use crate::channel_assist::sync;
pub use crate::channel_assist::telemetry;
pub use crate::channel_assist::types;

pub use classify::{
    is_actionable, parse_classification, ChannelClassifyConfig, ChannelClassifyWorker,
    Classification, ClassifyLlm, ClassifyPassOutcome, EvalFixture, EvalReport, LabelScore,
    RouterClassifyLlm, CHANNEL_CLASSIFY_OPERATION, LABELS,
};
pub use completion_port::ReconcileCompletionSink;
// parse_distill_output stays off this list: it is fixture-gated at the
// definition, and an ungated in-crate re-export is E0432 in every
// default-features build. Reach it at `assist::distill::parse_distill_output`
// with the feature on.
pub use distill::{
    ChannelDistillConfig, ChannelDistillWorker, DistillOutput, CHANNEL_INGEST_DISTILL_OPERATION,
    INTENT_TAXONOMY, MAX_DISTILL_ATTEMPTS,
};
pub use quality_budgets::{
    today_projection_latency_budget_ms, DEFAULT_TODAY_PROJECTION_LATENCY_BUDGET_MS,
};
pub use reconcile::{
    reconcile_annotation, reconcile_annotation_with_provider_changes, runtime_snapshot_json,
    ChannelReconcileConfig, ChannelReconcileWorker, ReconcileDecision, ReconcilePassOutcome,
};
pub use reply_draft::{
    parse_draft_json, ReplyDraftLlm, RouterReplyDraftLlm, CHANNEL_REPLY_DRAFT_OPERATION,
};
pub use writing_preferences::{
    derive_edit_preferences, normalize_statement, promote_to_memory, remove_from_memory,
    sender_domain, MAX_WRITING_PREFERENCE_CHARS, WRITING_PREFERENCE_TIER,
};

// The 3.1 relocation tests that pinned shim≡seam item identity were removed
// with the shims (Phase 5 batch 4): the flat paths they compared against no
// longer exist. The policy/rule test modules below pin the seam's decisions
// directly.

#[cfg(test)]
mod classification_policy_tests {
    //! Plan 3.1: the classification policy table. The classifier moved
    //! behind this seam as a module; these NEW tests pin the extracted
    //! decisions themselves — the label vocabulary, the off-vocabulary
    //! coercion, confidence normalization, and the annotation state a
    //! classification lands in — so the seam owns a visible policy, not just
    //! a moved file. (The pre-existing in-module suites moved with the file
    //! and stay green unedited; this table is additive.)

    use super::classify;
    use super::classify::{is_actionable, parse_classification, state_for, LABELS};
    use super::types::MailAnnotationState;

    #[test]
    fn label_vocabulary_and_actionability_are_pinned() {
        // The v1 vocabulary is exactly these four, mutually exclusive; only
        // needs_reply/follow_up are actionable (can produce a Today card).
        assert_eq!(LABELS, ["needs_reply", "follow_up", "fyi", "no_action"]);
        assert!(is_actionable("needs_reply"));
        assert!(is_actionable("follow_up"));
        assert!(!is_actionable("fyi"));
        assert!(!is_actionable("no_action"));
        assert!(!is_actionable("urgent"));
    }

    #[test]
    fn model_output_normalization_table() {
        // (raw model output, expected label, expected confidence)
        let table: &[(&str, &str, f64)] = &[
            // In-vocabulary label passes through with its confidence.
            (
                r#"{"label":"needs_reply","confidence":0.9}"#,
                "needs_reply",
                0.9,
            ),
            (
                r#"{"label":"follow_up","confidence":0.8}"#,
                "follow_up",
                0.8,
            ),
            (r#"{"label":"fyi","confidence":0.9}"#, "fyi", 0.9),
            // Missing confidence defaults to a conservative 0.5.
            (r#"{"label":"no_action"}"#, "no_action", 0.5),
            // Off-vocabulary label is coerced to fyi and the confidence is
            // NOT trusted (capped at 0.4).
            (r#"{"label":"urgent","confidence":0.95}"#, "fyi", 0.4),
            // Labels are lowercased before the vocabulary check.
            (
                r#"{"label":"NEEDS_REPLY","confidence":0.75}"#,
                "needs_reply",
                0.75,
            ),
            // Confidence is clamped to [0, 1].
            (r#"{"label":"fyi","confidence":1.7}"#, "fyi", 1.0),
            (r#"{"label":"fyi","confidence":-0.2}"#, "fyi", 0.0),
            // A ```json fence around the object is stripped.
            (
                "```json\n{\"label\":\"fyi\",\"confidence\":0.6}\n```",
                "fyi",
                0.6,
            ),
            // Stray prose before/after the object is tolerated (the first
            // balanced JSON object wins).
            (
                "Sure! {\"label\":\"follow_up\",\"confidence\":0.7} hope that helps",
                "follow_up",
                0.7,
            ),
        ];
        for (raw, expected_label, expected_confidence) in table {
            let parsed =
                parse_classification(raw).unwrap_or_else(|e| panic!("parse failed for {raw}: {e}"));
            assert_eq!(&parsed.label, expected_label, "raw: {raw}");
            assert!(
                (parsed.confidence - expected_confidence).abs() < 1e-9,
                "raw: {raw}, confidence: {}",
                parsed.confidence
            );
        }

        // Non-JSON output is an error, never a silent label.
        assert!(parse_classification("no json here at all").is_err());
    }

    #[test]
    fn state_policy_actionable_and_confident_promotes_to_a_today_card() {
        let classification = |label: &str, confidence: f64| {
            parse_classification(&format!(
                r#"{{"label":"{label}","confidence":{confidence}}}"#
            ))
            .expect("fixture classification parses")
        };
        // Actionable + at/above the high-confidence bar → needs_approval.
        assert_eq!(
            state_for(&classification("needs_reply", 0.7)),
            MailAnnotationState::NeedsApproval
        );
        assert_eq!(
            state_for(&classification("follow_up", 0.95)),
            MailAnnotationState::NeedsApproval
        );
        // Actionable but below the bar stays quiet.
        assert_eq!(
            state_for(&classification("needs_reply", 0.69)),
            MailAnnotationState::Classified
        );
        // Non-actionable stays quiet however confident.
        assert_eq!(
            state_for(&classification("fyi", 0.99)),
            MailAnnotationState::Classified
        );
        // The off-vocabulary coercion (fyi at ≤0.4) can never promote.
        let coerced = parse_classification(r#"{"label":"urgent","confidence":0.99}"#)
            .expect("off-vocabulary fixture parses");
        assert_eq!(state_for(&coerced), MailAnnotationState::Classified);
    }

    #[test]
    fn eval_scoring_is_pure_per_label_precision_recall() {
        // The Phase-8 eval gate: (gold, predicted) pairs → accuracy +
        // per-label precision/recall. Pure and deterministic, so the seam
        // can pin the arithmetic.
        let report = classify::score(&[
            ("needs_reply".to_string(), "needs_reply".to_string()),
            ("needs_reply".to_string(), "fyi".to_string()),
            ("fyi".to_string(), "fyi".to_string()),
        ]);
        assert_eq!(report.total, 3);
        assert_eq!(report.correct, 2);
        assert!((report.accuracy - 2.0 / 3.0).abs() < 1e-9);
        let needs_reply = &report.per_label["needs_reply"];
        assert_eq!(needs_reply.support, 2);
        assert_eq!(needs_reply.predicted, 1);
        assert_eq!(needs_reply.true_positive, 1);
        assert!((needs_reply.precision - 1.0).abs() < 1e-9);
        assert!((needs_reply.recall - 0.5).abs() < 1e-9);
        let fyi = &report.per_label["fyi"];
        assert!((fyi.precision - 0.5).abs() < 1e-9);
        assert!((fyi.recall - 1.0).abs() < 1e-9);
    }
}

#[cfg(test)]
mod reconcile_rule_tests {
    //! Plan 3.1: the reconciliation-rule matrix. The rules moved behind this
    //! seam as a module; these NEW tests pin the retire / supersede /
    //! require-review / stale matrix as one table — every decision variant
    //! against the evidence that triggers it and the state it lands in — so
    //! the seam owns a visible rule set, not just a moved file. (The
    //! pre-existing in-module suites moved with the file and stay green
    //! unedited; this matrix is additive.)

    use std::time::Duration;

    use serde_json::json;

    use super::reconcile::{reconcile_annotation_with_provider_changes, ReconcileDecision};
    use super::types::{
        MailAnnotationState, MailMessageMeta, MailRecordOrigin, MailThreadAnnotation,
        MessageDirection, ProviderThreadChange, ProviderThreadChangeKind,
        MAIL_ASSIST_SCHEMA_VERSION,
    };

    fn annotation(
        label: &str,
        follow_up_kind: &str,
        owner: &str,
        evidence_at: i64,
    ) -> MailThreadAnnotation {
        MailThreadAnnotation {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: "a1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            thread_id: "t1".to_string(),
            lane: Default::default(),
            state: MailAnnotationState::NeedsApproval,
            label: Some(label.to_string()),
            confidence: Some(0.9),
            reason: None,
            evidence_refs: vec!["thread:t1".to_string()],
            evidence_message_id: Some("m1".to_string()),
            evidence_message_at: Some(evidence_at),
            classification_input_revision: Some(1),
            semantic_features: None,
            proposed_action: Some(json!({
                "follow_up_kind": follow_up_kind,
                "action_owner": owner,
            })),
            provenance: None,
            created_at: evidence_at,
            updated_at: evidence_at,
        }
    }

    fn message(id: &str, at: i64, direction: MessageDirection) -> MailMessageMeta {
        MailMessageMeta {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            account_email: None,
            thread_id: "t1".to_string(),
            message_id: id.to_string(),
            provider_cursor: None,
            label_ids: Vec::new(),
            subject: Some("Follow-up".to_string()),
            from_name: None,
            from_address: None,
            to_domains: Vec::new(),
            cc_domains: Vec::new(),
            internal_date: at,
            observed_at: at,
            direction: Some(direction),
            summary: Some("A newer message arrived.".to_string()),
            intent: Some("reply".to_string()),
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: None,
            distill_contract_version: None,
            distilled_at: None,
            distill_revision: None,
            distill_state: Default::default(),
            distill_attempts: 0,
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
        }
    }

    fn trashed_thread_change(at: i64) -> ProviderThreadChange {
        ProviderThreadChange {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: "c1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            thread_id: "t1".to_string(),
            message_id: None,
            kind: ProviderThreadChangeKind::LabelsAdded,
            thread_removed: false,
            label_ids: vec!["TRASH".to_string()],
            current_label_ids: vec!["TRASH".to_string()],
            provider_cursor: None,
            observed_at: at,
        }
    }

    const STALE_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

    #[test]
    fn decision_matrix_matches_the_pinned_rules() {
        // Far enough past every evidence timestamp that the 30-day stale
        // window is the only thing that can fire for quiet threads.
        let now = 10_000_000_000_i64;
        // (name, annotation, newer messages, provider changes, expected)
        let cases: &[(
            &str,
            MailThreadAnnotation,
            Vec<MailMessageMeta>,
            Vec<ProviderThreadChange>,
            ReconcileDecision,
        )] = &[
            (
                "terminal state is never revisited",
                {
                    let mut a = annotation("needs_reply", "needs_reply", "owner", 1_000);
                    a.state = MailAnnotationState::Completed;
                    a
                },
                vec![message("m2", 2_000, MessageDirection::Outbound)],
                Vec::new(),
                ReconcileDecision::KeepActive {
                    reason: "terminal_or_quiet_state",
                },
            ),
            (
                "owner-owed work + a newer outbound send retires as completed",
                annotation("needs_reply", "needs_reply", "owner", 1_000),
                vec![message("m2", 2_000, MessageDirection::Outbound)],
                Vec::new(),
                ReconcileDecision::Complete {
                    reason: "owner_or_agent_sent_after_evidence",
                    resolution_message_id: "m2".to_string(),
                    resolution_message_at: 2_000,
                },
            ),
            (
                "provider trashing the thread retires as completed",
                annotation("fyi", "none", "unknown", 1_000),
                Vec::new(),
                vec![trashed_thread_change(2_000)],
                ReconcileDecision::CompleteProviderChange {
                    reason: "provider_thread_trashed_or_spam",
                    change_id: "c1".to_string(),
                    change_at: 2_000,
                },
            ),
            (
                "a newer non-outbound message on a pending draft requires review",
                {
                    let mut a = annotation("needs_reply", "needs_reply", "owner", 1_000);
                    a.state = MailAnnotationState::DraftReady;
                    a
                },
                vec![message("m2", 2_000, MessageDirection::Inbound)],
                Vec::new(),
                ReconcileDecision::Stale {
                    reason: "newer_message_requires_draft_review",
                },
            ),
            (
                "a counterparty reply that resolves waiting-on completes",
                annotation("follow_up", "waiting_on", "counterparty", 1_000),
                vec![{
                    // An inbound that no longer asks for anything: no reply
                    // hint, no follow-up hint, a closing intent.
                    let mut m = message("m2", 2_000, MessageDirection::Inbound);
                    m.intent = Some("transactional".to_string());
                    m
                }],
                Vec::new(),
                ReconcileDecision::Complete {
                    reason: "counterparty_replied_to_waiting_on",
                    resolution_message_id: "m2".to_string(),
                    resolution_message_at: 2_000,
                },
            ),
            (
                "a counterparty message that changes the follow-up supersedes",
                {
                    let mut a = annotation("follow_up", "waiting_on", "counterparty", 1_000);
                    a.proposed_action = Some(json!({
                        "follow_up_kind": "waiting_on",
                        "action_owner": "counterparty",
                    }));
                    a
                },
                vec![{
                    let mut m = message("m2", 2_000, MessageDirection::Inbound);
                    m.needs_reply_hint = true;
                    m
                }],
                Vec::new(),
                ReconcileDecision::Supersede {
                    reason: "counterparty_message_changes_followup",
                    resolution_message_id: "m2".to_string(),
                    resolution_message_at: 2_000,
                },
            ),
            (
                "any other newer evidence supersedes",
                annotation("follow_up", "none", "unknown", 1_000),
                vec![message("m2", 2_000, MessageDirection::Inbound)],
                Vec::new(),
                ReconcileDecision::Supersede {
                    reason: "newer_evidence_supersedes",
                    resolution_message_id: "m2".to_string(),
                    resolution_message_at: 2_000,
                },
            ),
            (
                "quiet past the stale window goes stale",
                annotation("follow_up", "none", "unknown", 1_000),
                Vec::new(),
                Vec::new(),
                ReconcileDecision::Stale {
                    reason: "stale_window_elapsed",
                },
            ),
            (
                "quiet inside the stale window stays active",
                {
                    let a = annotation("follow_up", "none", "unknown", now - 1_000);
                    a
                },
                Vec::new(),
                Vec::new(),
                ReconcileDecision::KeepActive {
                    reason: "no_newer_reconciling_evidence",
                },
            ),
        ];
        for (name, annotation, newer, changes, expected) in cases {
            let decision = reconcile_annotation_with_provider_changes(
                annotation,
                newer,
                changes,
                now,
                STALE_AFTER,
            );
            assert_eq!(decision, *expected, "rule case: {name}");
        }
    }

    #[test]
    fn target_state_matrix_retire_supersede_and_require_review() {
        use MailAnnotationState::*;
        // Completion from a draft/inserted state first records the send, and
        // only a completion observed on SentDetected lands in Completed.
        let complete = ReconcileDecision::Complete {
            reason: "owner_or_agent_sent_after_evidence",
            resolution_message_id: "m2".to_string(),
            resolution_message_at: 2_000,
        };
        assert_eq!(complete.target_state(DraftReady), Some(SentDetected));
        assert_eq!(complete.target_state(Inserted), Some(SentDetected));
        assert_eq!(complete.target_state(SentDetected), Some(Completed));
        assert_eq!(complete.target_state(NeedsApproval), Some(Completed));
        // Provider-change completion is a straight completion.
        assert_eq!(
            ReconcileDecision::CompleteProviderChange {
                reason: "provider_thread_trashed_or_spam",
                change_id: "c1".to_string(),
                change_at: 2_000,
            }
            .target_state(NeedsApproval),
            Some(Completed)
        );
        // Supersession retires the annotation as superseded.
        assert_eq!(
            ReconcileDecision::Supersede {
                reason: "newer_evidence_supersedes",
                resolution_message_id: "m2".to_string(),
                resolution_message_at: 2_000,
            }
            .target_state(NeedsApproval),
            Some(Superseded)
        );
        // Stale and routing-retracted both land in Stale (a retraction is a
        // rule change, not an expiry — same target state, different reason).
        assert_eq!(
            ReconcileDecision::Stale {
                reason: "stale_window_elapsed",
            }
            .target_state(NeedsApproval),
            Some(Stale)
        );
        assert_eq!(
            ReconcileDecision::RoutingRetracted {
                reason: "required_action_no_longer_derived",
            }
            .target_state(NeedsApproval),
            Some(Stale)
        );
        // Keep-active transitions nothing.
        assert_eq!(
            ReconcileDecision::KeepActive {
                reason: "no_newer_reconciling_evidence",
            }
            .target_state(NeedsApproval),
            None
        );
    }
}
