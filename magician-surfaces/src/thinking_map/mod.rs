//! Live Thinking Map (Phase 1a) — pure, dormant domain model + operation
//! protocol.
//!
//! Finalized speech continuously updates a *typed, revisable* model of the
//! user's thinking: ideas/facts/questions/decisions/risks/actions/metrics/
//! groups are first-class nodes; later speech can correct/supersede/reconnect
//! earlier nodes; every change is a bounded, validated operation against a
//! known revision.
//!
//! This module is the typed core + deterministic reducer + a durable,
//! scope-owned persistent store (Phase 2a: append-only event log + snapshots).
//! It contains NO API, NO provider/LLM, and NO UI, and is NOT wired into the
//! runtime — no route/service registration anywhere. The store is a usable
//! library type only; higher layers are built separately.

pub use magician::magician_v2::thinking_map_models as models;
pub use magician::magician_v2::thinking_map_operations as operations;
pub use magician::magician_v2::tutor_map_context as tutor_context;

pub mod consolidation;
pub mod context;
pub mod coordinator;
pub mod errors;
pub mod export;
pub mod interpreter;
pub mod llm_router_adapter;
pub mod reducer;
pub mod replay;
pub mod store;
pub mod validation;

pub use consolidation::{consolidate, ConsolidationResponse};
pub use context::{
    build_context, CompactClarification, CompactNode, ContextBudget, InterpreterContext, MapDigest,
};
pub use coordinator::{SourceBinding, ThinkingMapSessionCoordinator};
pub use errors::{ThinkingMapError, ThinkingMapResult};
pub use export::{render_markdown, ExportOptions};
pub use interpreter::{
    interpret, translate, InterpretIntent, InterpretProgressSink, InterpretStage,
    InterpretationResponse, InterpreterLlm, NoProgress, ProposedOperation, Utterance,
};
pub use llm_router_adapter::RouterInterpreterLlm;
pub use models::{
    new_clarification_id, new_edge_id, new_map_id, new_node_id, new_proposal_id,
    AppliedEnvelopeRecord, AssertionOrigin, Clarification, ClarificationId, ClarificationState,
    EdgeId, EdgeKind, EpistemicState, MapId, MapLifecycle, NodeId, NodeKind, Position, PromotedRef,
    PromotionKind, ProposalId, ProposalState, RestructureProposal, SharedViewState, SourceRef,
    SpeakerRef, ThinkingEdge, ThinkingMap, ThinkingMapSource, ThinkingNode, ViewLens,
    APPLIED_ENVELOPE_LEDGER_CAP, THINKING_MAP_SCHEMA_VERSION,
};
pub use operations::{
    new_envelope_id, MapOperation, MapOperationEnvelope, ModelTraceRef, OperationActor,
};
pub use reducer::{apply_envelope, semantic_hash, ApplyOutcome};
pub use replay::{diff_maps, MapDiff, RepairReport, StartupRepairSweep};
pub use store::{
    MapEvent, MapManifest, MapSummary, NodePreview, NodePreviewEdge, NodePreviewNode, StoreResult,
    ThinkingMapStore, ThinkingMapStoreError, NODE_PREVIEW_MAX_NODES,
};
pub use tutor_context::{
    build_tutor_map_context, tutor_map_context_registry, TutorMapContextBinding,
    TutorMapContextRegistry, DEFAULT_TUTOR_MAP_CONTEXT_BUDGET_CHARS, TUTOR_MAP_CONTEXT_TTL_MS,
};
