// MagicianV2: Advanced Tool Discovery System
// Staged implementation following natural query processing flow

// Core infrastructure
pub mod browser_engine_analytics;
pub mod presentation_identity;
pub mod progress_channel_seam;
pub mod prompt_identity;
pub use magician_core::prompts;

// Installable, scoped app-platform contracts. Phase 0 keeps this domain
// dormant: no routes, storage, or runtime dispatch are wired by merely
// compiling these transport-neutral records.
pub mod api_scope;
pub mod apps;
pub mod auth;

// Execution engine for browser plans
pub mod execution;

// Structured-decision plane host wiring (typed Choice/Score/Noul judgments)
pub mod decision_host;
pub(crate) mod decisions;

// Stage 1: Query Understanding & Analysis (implemented)
pub mod query_analysis;

// Integration layer - Orchestration with V1 compatibility
pub mod monitor_support;
pub mod monitors;
pub mod observe_connectors;
pub mod orchestrator;

// Slot/elicitation management shared across planning & execution
pub mod elicitation;

// Structured slot graph representation
pub mod slot_graph;

// Confidence calculations for slots/workflows
pub use magician_core::confidence;

// Ask loop components (budget, clarifier, pause/resume)
pub mod ask_loop;

// State tracker abstractions
pub mod state_tracker;

// Stage 2: Strategy-based exploration system (implemented)
pub mod strategy;

// V2 Storage System - dedicated, not shared with V1
pub mod storage;

// Scope-aware inventory, compaction, and retention for Magician-owned stores.
pub mod storage_governance;

// Task 10 typed object-owner kit (local directory layouts stay canonical).
pub mod object_owners;

// Task 11 Parquet dataset kit (local dt=* layouts stay canonical).
pub mod dataset_owners;

// Task 12 work-spine kit (local task/execution/plan/pause/list-index layouts stay canonical).
pub mod work_owners;

// Task 13 chat and progress kit (local session/transcript/progress layouts stay canonical).
pub mod chat_owners;

// Task 14 agent, memory, learning, and index kit (local layouts stay canonical).
pub mod agent_owners;

// Task 15 SQLite/DuckDB kit (local database files stay canonical).
pub mod database_owners;

// Task 16 system, device, and secret kit (local layouts stay canonical).
pub mod system_owners;

// Task 16A subprocess and skill storage kit (workdirs and skill working dirs
// stay ephemeral until accepted).
pub mod subprocess_owners;

// Task 17 cross-owner backup, restore, integrity walker, and delayed GC.
// Not invoked at default startup.
pub mod recovery;

// Task 18 Track A acceptance: remote-ready while locally active.
// Default startup stays local_embedded. Not an activation surface.
pub mod track_a_acceptance;

// Task 18A Decision Gate 3 performance and capacity budgets.
pub mod gate3;

// Task 19 Track B operator activation. Not invoked at default startup.
pub mod storage_activation;

// Task 20 local/remote profile acceptance. Default startup stays local.
pub mod profile_acceptance;

// Process-wide StorageRuntime handle. Magician-bin installs it; libraries
// must not open a second backend from MAGICIAN_ROOT_DIR.
pub mod process_storage;

// Live workspace I/O onto cataloged owner kits.
pub mod typed_io;

// Intent-aware message processing
pub mod intent_handler;

// Shared service registry for dependency injection
pub mod services;

// Unified secret management shared by execution/runtime paths
pub mod secrets;

// Process-wide registry of extra "system-root" directories declared in
// `tool-runtime-config.yaml :: registry.paths`.
pub mod cloudflare_access;
pub mod cors;
pub mod critical_delivery_settings;
pub mod device_bridge;
pub mod device_governance;
pub mod device_pairing;
pub mod hitl_delivery;
pub mod mcp_oauth;
pub mod mobile_push;

// Engagement authority — capability as a property of (agent, context).
// OPC Workstream B Phase 0: the authority carrier's record and store.
pub mod engagements;

// Engagement-scoped retrieval containment — §5A.2. The seam between the
// authority carrier and retrieval: an execution bound to one engagement must
// not surface another's context, and unlabelled context is not proof of
// neutrality.
pub mod engagement_retrieval;

// Test-only: reads this crate's own source so a module header that claims a
// CALL SITE — "runs on a cadence", "every send passes through here" — can be
// pinned by a count instead of by prose nobody re-checks.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod doc_wiring_scan;

pub use magician_core::config_extras;

// Deployment-level LLM pricing file (`llm_pricing.json`): load at startup,
// install over the built-in pricing table.
pub mod llm_pricing_config;

// Stack-safe shared traversal for externally supplied JSON/tool payloads.
pub use magician_core::json_traversal;

// Operation-owned logical-context adapters, bootstrap validation, and the
// Phase 7 production logical-request runner boundary.
pub mod llm_chunking;
pub mod llm_dispatch_seam;

// Tooling adapters / boundary helpers
pub mod thinking_map_models;
pub mod thinking_map_operations;
pub mod today_projection_cache;
pub mod tooling;

// Surface-neutral, structure-preserving model/display/spoken projections of
// canonical tool results. Dispatch integrations live at their respective
// runtime seams; this module intentionally contains only the pure contract.
pub mod tool_result_projection;
pub mod tool_result_runtime;

// Atomic, scope-bound storage for complete post-redaction tool results.
// Projection and surface wiring consume this module in later phases.
pub mod tool_result_materialization;

// Test utilities - shared mocks and helpers
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_utils;

pub mod task_lanes;
pub mod task_run_factory;

// Stage 3: V2 Tool Matching with category-aware 4-tier filtering (in progress)
pub mod tool_matcher;

// Real-time event broadcasting for WebSocket streaming
pub mod realtime_events;
pub mod resurfacing_seam;

// Core attention service — the corpus-generic resurfacing + attention
// learning engines relocated from magician-comms behind the
// resurfacing_seam boundary (plan workstream 3.0). Comms-coupled adapters
// stay satellite-side implementing these traits.
pub mod attention;

// Per-(principal, workspace) transport event log — persists every
// `RuntimeTransportEvent` so refresh shows the same rows that were
// live in-memory before the page reload. See module docs for retention
// policy (`EVENTS_RETENTION_WINDOW_MS` / `EVENTS_RETENTION_MIN_COUNT`).
pub mod transport_log;

// Personal Tutor guided-screen workflow contracts.
pub mod tutor;
pub mod tutor_map_context;

// The VibeDev cockpit, behind the lane seam as one module tree (plan 3.4):
// the project store, the durable dispatch intents, the run service and the
// `@vibedev` rail. `chat/service.rs` keeps turn orchestration and calls the
// tree's seam-owned decisions. The pre-3.4 flat-path `pub use` shims were
// removed in Phase 5 (removal inventory batch 2, 2026-08-28).
pub mod vibedev;

// User-visible note provider settings and Markdown/SilverBullet workspace writes.
pub mod notes;
pub(crate) mod notes_hybrid;
pub(crate) mod notes_links;
pub(crate) mod notes_search_index;

// The notes/tasks projection seam (plan 2.3): the product logic behind task
// pages and selection capture, one-way over the notes provider above. The
// provider keeps the registry, selection/fallback, and write orchestration.
pub mod notes_projection;

// Owner-edited taste profile note: settings, provider-backed loading, and the
// injectable snapshot consumed by prompt assembly.
pub mod taste_profile;

// Capture: distilling finished sessions into taste proposals the owner
// approves. Holds all machine state, so the proposals note stays write-only.
pub mod taste_capture;

// Slice 3: choosing preferences by applicability rather than cosine
// similarity. Pure two-stage selection; the judge call lives with its caller.
pub mod memory_applicability;

// Scope-owned UI preferences shared by browser and desktop webviews.
pub mod ui_preferences;

// API-backed canonical workspace-storage provider selection.
pub mod local_generation_settings;
pub mod privacy_processing_settings;
pub mod workspace_storage_settings;

// Temporary feature kill switches during runtime_v3 bring-up

// Chat mode — conversational LLM interface (Phase 1)
pub mod chat;
pub use magician_core::history;

// Shared, surface-neutral staged retrieval for turn-scoped memory,
// procedures, and other prompt context.
pub mod context_retrieval;
pub mod counterparty_consumers;
pub mod counterparty_store;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod counterparty_store_tests;
pub mod counterparty_types;
pub mod outbound;

/// Compatibility facade for the pre-split counterparty namespace. The storage,
/// consumer, and type owners are flat modules now; callers may still use the
/// cohesive vocabulary without restoring the former directory module.
pub mod counterparties {
    pub use super::counterparty_consumers::*;
    pub use super::counterparty_store::*;
    pub use super::counterparty_types::*;
    pub use super::outbound::*;
}

// Consumer-channel bot supervision/runtime

// API mining infrastructure (Phase 1 — trace capture/storage, no callers yet)
pub mod api_mining;

// AgentSkills v1 loader + activation (Phase 1 — pure-instruction + tool-backed
// procedure skills; personality-mode + agent-definition routes added in
// later phases of the migration plan).
pub mod skills;

// Agent system foundation (Phase 0 — TRUE_AGENTS)
pub mod agents;

// Harness capability surface for autonomous personal agents
pub mod harness;

// Generative Agent UI — MUIJ types, storage, delta emitter (GAUI)
pub mod gaui;

// Pipeline module — typed artifacts and store for agent pipeline stages
pub mod pipeline;

// Artifact lifecycle control plane — catalog, policy, sanitization, scheduling
pub mod artifacts;

// Consent per outcome rather than per act: durable, bounded, revocable
// pre-authorisation resolved at dispatch. Sibling of `resource_authority` —
// that one governs how much of a commodity an act may spend, this one governs
// what class of outcome it may cause. Default-off.
pub mod approval_envelopes;

// What the provider did with an act after we dispatched it. Without this every
// live send stays `dispatch_unknown` forever, which is why reconciliation is a
// named gate for turning capture mode off. Terminal states never resurrect.
pub mod delivery;

// The join between what a provider reported and who we may no longer contact.
// It lives outside both because neither should import the other: a receipt
// ledger holds no contact policy, and a contact register knows no webhooks.
pub mod delivery_hygiene;

// Bounces do not only arrive by webhook — they come back AS MAIL. Reads RFC
// 3464 delivery-status notifications out of the sending inbox and turns them
// into receipts, so a hard bounce reaches the ledger without the provider
// exposing an event stream it does not have.
pub mod delivery_receipts;

// Identities we must not contact, and why. Global to the owner by default: an
// opt-out in one programme is not consent in another. Reads FAIL CLOSED — an
// unreadable register is an error, never "nobody is suppressed".
pub mod suppression;

// The document channel of an engagement — references, never copies, with no
// access model of its own: who may read derives from the engagement. Phase 1 is
// the container only; there is deliberately no way to share it yet.

// What is owed, and by when — promises with a deadline and a direction.
// Lapsing is derived from the clock, so the register cannot depend on a sweep
// having run; catching what nobody remembered is the whole job.
pub mod obligations;

// Capability that flows from the WORK as well as the actor: `effective = agent ∩
// work_context`. A program names what its work needs; naming never grants, so
// intersection can only narrow and an unheld capability becomes a delegation
// pointer rather than a new permission.
/// What identity research concluded, written into the counterparty register as
/// an unverified guess. A **coordinator**: research keeps knowing nothing about
/// counterparties, and the register keeps knowing nothing about chat.
pub mod contact_research;
/// Who introduced us to whom, and what we owe them for it. A **coordinator**:
/// it joins the counterparty register to the obligation register so neither
/// primitive has to learn about the other.
pub mod introductions;
/// The four recipient checks that belong below the agent rather than in its
/// judgement — a duplicate contact by another work, a reply already waiting on
/// us, a jurisdiction port, and a recorded erasure request. A **coordinator**:
/// it joins the outward-assertion register, the scheduling record and the
/// suppression register so none of the three learns about the others. One
/// gate, four rules, one typed answer — an outward send asks one question.
pub mod recipient_compliance;
pub mod work_context;

// Terms that appeared in a conversation, recorded and never authored. Nothing
// here negotiates; being unable to negotiate is not a reason to be blind to
// negotiation happening. Nothing may be restated outward until a named person
// confirms it.
pub mod commitments;

// Who may see something: a named, enumerable set of identities with a lifetime.
// There is deliberately no public variant — if you cannot list who is in it, it
// is publication, which is a different consequence class.
pub mod audience;

// Honest log reading, shared by every append-only store: absent is the only
// error that means "empty", and a torn tail is an append that never happened
// while a torn middle is corruption that refuses to fold.
pub mod jsonl;

// Capability grants for audience members: one identity, one link, expiring,
// revocable. The secret is never stored — hash only. Feeds the access log's
// possession-not-identity record.
pub mod share_links;

// Which claims an artifact revision carries. What was BUILT, where the outward
// assertions store records what was SENT — correction propagation needs both,
// because a corrected figure lives on in drafts and decks too.
pub mod claim_manifest;

// What FILLS the obligation register: the composition that runs the data-room
// follow-up sweep and the scheduling silence sweep against real activity, plus
// the cadence that runs it. Lives OUTSIDE `obligations` on purpose — Module D
// must not import the modules that feed it, or every future emitter would mean
// editing the register.

// What actually worked: outcome observations, recorded as work happens. Phase 1
// is deliberately record-only — the data cannot be reconstructed afterwards, and
// the guardrails (mature silence, cohort keys, delivery apart from response)
// come before any loop that reads them.

// Canonical V3 task/execution storage and reducers
pub mod artifact_v2;

// Generic, domain-agnostic work-understanding evidence substrate
// (`EvidenceRecord` + facets). Distilled from episodes alongside tier
// knowledge. See docs/components/magician/work-evidence-graph.md (impl plan
// archived at docs/archive/plans/2026-06-13-work-evidence-graph-phase-0-1.md).
pub mod evidence;

// Embedded analytics layer — schemaless event ingestion + schema catalog
pub mod analytics;

// Runtime services — long-lived external processes (Ollama, etc.) owned by
// the magician binary; lifecycle tied to boot/shutdown.
pub mod runtime;

// Runtime settings mutation primitives for UI-backed feature settings.
pub mod runtime_settings;

// Agent-growth learning substrate — audited learning observations,
// candidates, provenance, and inert decision logs.
pub mod learning;

// YAML-defined dashboard themes (font/palette/spacing/atmosphere/motion bundles).
// Used by published-surface rendering to give every dashboard a distinctive look
// regardless of source format.
pub mod dashboard_themes;

// Boot-time seed for the LLM Calls Overview demo dashboard. Owns the
// canonical seed-task id and the MUI-JSON payload (with live dataSource
// bindings) that gets published on first run of each scope.
pub mod dashboard_seed;

// Artifact-backed route publications for GAUI surfaces
pub mod surfaces;

// Resource Authority — double-entry ledger, spend tokens, hard gate
pub mod resource_authority;

// Central user-request service — ask-the-human primitive
pub mod user_requests;
pub mod verification_codes;

// Feed read-model for Today, Activity, and thread surfaces
pub mod feed;

// Provider-neutral content acquisition for user-defined feeds. Discovery
// sources (comms, RSS, search) and content readers normalize into contracts
// that deliberately know nothing about feed or monitor product semantics.
pub mod content_sources;

// One scoped policy + boot-local ledger for bounded automatic Observe catch-up.
pub mod observe_catchup;

// Provider-neutral attention routing vocabulary and deterministic router used
// by comms classification, resurfacing curation, Today projection, and lane
// observability.
pub use magician_core::attention_funnel;

// Complete direct-open payload for HITL affordances outside the Attention feed.
pub use magician_core::hitl;
pub mod hitl_deprecation_metrics;

// Adapter-neutral route-event storage and grouped funnel observability.
pub use magician_core::attention_funnel_store;

// Internal cursor lane-query facade shared by Today, Follow-ups, Worth a look,
// and attention inbox pagination.
pub mod attention_lane_facade;

// Destination-owner validation, idempotency identity, and the lane-record
// adapter for app-proposed attention candidates (plan 1.1 attention port).
// Validation and rendering only — durable staging and the dispatch outbox
// arrive with the 3.1 consumer, so no existing lane changes.
pub mod attention_lane_contribution;

// Destination-owner validation, idempotency identity, and the signed-apply
// seam for app-proposed learning-candidate decisions (plan 2.5 apply path).
// The port reaches the real `LearningStore` transition behind an owner-signed
// decision envelope; proposal staging and the dispatch outbox arrive with the
// destination consumer, and the first-party transition API keeps running
// unchanged beside it.
pub mod learning_decision_contribution;

// Dedicated owner-signed apply seam for the authoritative claims and
// commitments registers. This is intentionally separate from the generic
// hypothesis-only contribution terminal and dispatches only to the canonical
// expected-revision transition methods.
pub mod claims_decision_contribution;
pub mod meeting_control_contribution;

// Shared `gws` (Google Workspace CLI) subprocess helpers — binary
// resolution + stdout-JSON error extraction, used by the meetings surface
// and the Channel Assist Gmail client.
pub use magician_core::gws_cli;

pub mod channel_types;

// Social network for agents (Phase 0)
pub mod social;

// Curated execution-panel read model
pub mod execution_panel;

// Persisted UI thread model for Today/thread navigation
pub mod ui_threads;

// Realtime media / control rails — shared substrate for browser TTS/STT,
// camera/mic capture, screen + pointer surfaces, ambient audio, and
// realtime voice. Phase 0 = session registry + capability/permission
// advertisement + per-channel lifecycle events. See
// `docs/archive/plans/2026-05-15-realtime-media-control-rails.md`.
pub mod media_seam;
// The audio-notes UX seam (plan 3.5): the media-adjacent product decisions
// (archive layout, dated-page contract, listing order, receipt projection)
// behind the notes provider's audio-note write path. The provider keeps the
// registry/settings/write orchestration and re-imports these under the same
// names (the plan-2.3 `notes_projection` pattern).
pub mod audio_notes_seam;
// Glue between `magicllm::dispatch` (the global LLM dispatch queue) and the
// host's task store / runtime ledger. See
// `docs/plans/2026-05-26-llm-dispatch-queue-and-local-prep.md`.
pub mod dispatch_glue;

// Hard live-agent admission, RSS tripwire, and process-local resource gauges.
pub mod local_resource_governor;

// Bounded Magician-owned spawn_blocking admission (R12). Does not shrink
// Tokio max_blocking_threads.
pub use magician_core::blocking_admission::{
    blocking_admission_permits, blocking_admission_snapshot, configure_blocking_admission,
    spawn_blocking_admitted,
};

// Dedicated abortable runtime for request-path LanceDB searches.
pub use magician_vector_index::{
    build_lance_runtime, build_lance_runtime_with_threads, configure_lance_search_concurrency,
    configured_lance_runtime_mode, lance_runtime_mode, set_lance_runtime_handle,
    should_build_lance_runtime, LanceRuntimeMode,
};

// Future stages (to be implemented)
// pub mod dag_conversion; // Stage 4: Tree-to-DAG Conversion
// pub mod cache;          // Stage 6: Persistent Caching & Learning
// pub mod production;     // Stage 7: Production Integration

// Re-export main types for easier access
pub use agents::{AgentDefinition, AgentId, AgentMemoryService, AgentRoutingContext, AgentStorage};
pub use ask_loop::{
    AskDecision, AskLoopApi, BudgetLedger, BudgetLedgerError, BudgetPolicy, Channel,
    ClarifierLibrary, ClarifyRequest, ClarifyResponse, PauseReason, ResumeTriggerService,
    TaskComplexity,
};
pub use confidence::{ConfidenceConfig, ConfidenceService, ConfidenceSummary, ConfidenceWeights};
pub use elicitation::{
    ElicitationManager, ElicitationManagerBuilder, ObservationEvent, ObservationStatus,
    UnresolvedInput, UnresolvedInputUpdate, UpdateSource,
};
pub use gaui::{
    DefaultComponentRegistry, MuijComponent, MuijDelta, MuijDocument, MuijStorage,
    MuijStorageError, MuijValidationError,
};
pub use intent_handler::{IntentAwareProcessor, MessageHandlingResult};
pub use orchestrator::{
    v2_orchestrator::AnalysisMetadata, v2_orchestrator::ProcessingMetadata, MagicianV2Orchestrator,
    MagicianV2Response,
};
pub use prompts::{JsonPromptStorage, Prompt, PromptManager, PromptStore};
pub use query_analysis::{
    CategoryAnalysis, ComplexityAnalysis, ConversationContext, DependencyAnalysis,
    ExtractedEntities, QueryIntent, ResourceEstimate, SlotMatchAnalysis, UnifiedQueryAnalysis,
    UnifiedQueryAnalyzer,
};
pub use realtime_events::{
    AgentEventEnvelope,
    EventCategory,
    EventSeverity,
    EventTaxonomy,
    ExplorationSummary,
    RuntimeTransportBroadcaster,
    RuntimeTransportEvent,
    EVENT_TAXONOMY_TABLE,
    // NOTE: CancellationReason, ToolMatchStatus removed - events using them were removed
};
pub use services::MagicianV2Services;
pub use slot_graph::{ProvenanceRecord, ProvenanceSource, SlotRecord, SlotType};
pub use state_tracker::{
    AssetContent, AssetType, BudgetSpend, BudgetState, ConfidenceScore, ObservationAsset,
    SlotDelta, StageContext, StateBundle, WorkflowState,
};
pub use storage::{
    ExecutionRun, ExecutionRunDocument, FileV2Store, TurnDirection, V2ConversationStore, V2Slot,
    V2SlotStatus, V2StorageError, V2Turn, WaitingState,
};
pub use strategy::{
    AdaptiveStrategySelector, EntityMapper, ExplorationResult, ExplorationStrategy,
    GuidedSearchParams, GuidedSearchStrategy, MappingDetail, ParameterMappingResult,
    ResourceBudget, StrategyContext, StrategyType, TypeConversion,
};
pub use surfaces::{SurfaceRequest, SurfaceSpec, SURFACE_SCHEMA_VERSION};
pub use tool_matcher::{
    MatchingTier, TierScores, ToolCandidate, ToolMatchError, ToolMatchRequest, ToolMatchResult,
    ToolMatcherConfig, ToolMetadata, V2ToolMatcher,
};
