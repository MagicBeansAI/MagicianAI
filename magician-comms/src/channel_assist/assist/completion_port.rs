//! The reconciliation completion port (plan workstream 3.1 prerequisite (a),
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! Reconciliation is product-lane logic: it decides, from locally-distilled
//! evidence, that the owner discharged a follow-up. Attention learning is
//! lib-side infrastructure (`magician_v2::attention::learning`, relocated by
//! plan workstream 3.0) and must not be a named dependency of this crate's
//! product decisions — so the report crosses this small port instead.
//!
//! The port carries no attention vocabulary: `principal`/`workspace`/
//! `annotation_id`/`completed_at` are the whole surface. The binary wires the
//! implementation (`ReconcileAttentionLabeller` in `magician-api`), which
//! turns each report into a durable `ActionCompleted` attention label; since
//! the 3.0 relocation that implementation calls the lib-side attention
//! learning service. The wiring is unchanged by this extraction: the trait
//! text moved here from `reconcile.rs` verbatim, and every implementer now
//! names this path directly (the Phase 5 batch-4 shim removal retired the
//! old `channel_assist::reconcile::ReconcileCompletionSink` import path).
//!
//! Why the port exists at all (carried over from the trait's original doc):
//! the only positive actionability label the model can learn from is "the
//! owner did the thing this item asked for", and in-product approval captures
//! a small minority of that: the owner usually replies in their mail client,
//! where no click is available to record. Reconciliation is already watching
//! for exactly that evidence in order to retire the card.

/// Receives the completions this pass proved the owner performed.
///
/// Reconciliation stays provider-neutral and knows nothing about attention
/// learning; it only reports what it observed. The binary supplies an
/// implementation that turns each report into a durable label.
#[async_trait::async_trait]
pub trait ReconcileCompletionSink: Send + Sync {
    async fn record_owner_completion(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
        completed_at: i64,
    );
}
