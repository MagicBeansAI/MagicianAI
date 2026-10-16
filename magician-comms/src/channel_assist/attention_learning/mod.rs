//! Evidence-preserving, owner-scoped attention learning — comms side.
//!
//! Plan workstream 3.0 relocated the corpus-generic engine lib-side to
//! `magician::magician_v2::attention::learning`. Phase 5 (batch 5 of the
//! 2026-08-28 removal inventory) removed the glob re-export that mirrored
//! that tree under this path — import the engine types from the lib path
//! directly. What remains here is the comms-coupled worker surface:
//!
//! * `historical_bootstrap` — drains pre-live mail feedback into outcomes.
//! * `semantic_extraction` — backfills semantic envelopes for follow-ups.
//! * `rank_recompute` — resolves rank diagnostics against the canonical
//!   attention union projection (mail + resurfacing + learning stores).
//!
//! The rank-recompute job/result vocabulary stays re-exported (sourced from
//! the lib path) so consumers of `AttentionRankRecomputeWorker` can name the
//! types its signatures take without a second import root.

pub mod historical_bootstrap;
pub mod rank_recompute;
pub mod semantic_extraction;

pub use historical_bootstrap::{
    AttentionHistoricalBootstrapReport, AttentionHistoricalBootstrapWorker,
};
// The explicit list re-exports from the LIB path directly, pinning
// each name to the canonical library item by construction.
pub use magician::magician_v2::attention::learning::rank_recompute::{
    AttentionRankRecomputeEnqueueStatus, AttentionRankRecomputeGeneration,
    AttentionRankRecomputeJob, AttentionRankRecomputePauseReason,
    AttentionRankRecomputeQueueCounts, AttentionRankRecomputeReference,
    AttentionRankRecomputeResult, AttentionRankRecomputeRunReport, AttentionRankRecomputeStatus,
    ScheduleAttentionRankRecompute, ATTENTION_RANK_RECOMPUTE_REASON_MAX_CHARS,
    ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS, ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION,
    ATTENTION_RANK_RECOMPUTE_SERVED_UNIVERSE_SEMANTICS,
    ATTENTION_RANK_RECOMPUTE_WRONGLY_STALED_REASON,
};
// The worker is comms-side (it is constructed with the comms store), so it
// must come from the local module — the lib path above covers only the
// types and vocabulary.
pub use rank_recompute::AttentionRankRecomputeWorker;
pub use semantic_extraction::{
    SemanticExtractionInput, SemanticExtractionPauseReason, SemanticExtractionRunReport,
    SemanticExtractionWorker, SemanticFeatureExtractor, UnavailableSemanticFeatureExtractor,
};

#[cfg(test)]
mod relocation_tests {
    //! Plan workstream 3.0 moved the engine core lib-side; Phase 5 (batch 5)
    //! removed the glob mirror. These tests pin the remaining comms surface:
    //! the local `rank_recompute` worker module's vocabulary re-export must
    //! name the exact lib items the worker consumes.

    #[test]
    fn worker_vocabulary_reexports_the_lib_attention_service_types() {
        // The annotated bindings compile only when the comms path names the
        // exact lib type (a divergent copy would fail to type-check).
        let generation: crate::channel_assist::attention_learning::rank_recompute::
            AttentionRankRecomputeGeneration =
            magician::magician_v2::attention::learning::rank_recompute::
                AttentionRankRecomputeGeneration {
                    follow_up: 1,
                    worth_a_look: 2,
                };
        assert_eq!((generation.follow_up, generation.worth_a_look), (1, 2));
    }
}
