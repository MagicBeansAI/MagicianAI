//! Generic, domain-agnostic work-understanding evidence substrate.
//!
//! An [`EvidenceRecord`] is a provenance-backed distillation of one unit of
//! observed work. It is a *sibling* of memory-tier knowledge — both are
//! distilled from the same `V3EpisodeRecord` source via the same consolidation
//! hook — not a derivation of tiers. Tier knowledge is compact, prompt-injected
//! "what to remember"; evidence is structured "what work happened", for
//! reporting / Career-Copilot-style read paths.
//!
//! Domain is a derived, multi-label [`Facet`] classification (open vocabulary,
//! LLM-proposed, user-overridable), **not** a baked-in record type: "work" is
//! one facet value. This keeps the substrate generic so the Visual Semantic
//! Eventing scout can later feed it as another producer.
//!
//! See `docs/components/magician/work-evidence-graph.md` (impl plan archived at
//! `docs/archive/plans/2026-06-13-work-evidence-graph-phase-0-1.md`) and the
//! design doc `docs/plans/2026-03-17-work-evidence-graph-design.md`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::magician_v2::agents::AgentMemoryService;
use crate::magician_v2::artifact_v2::memory::V3EpisodeRecord;
use crate::magician_v2::prompts::PromptManager;
use crate::magician_v2::query_analysis::operation_llm_router::{
    LLMOperation, OperationLlmRouter, SimplifiedLLMResponse,
};

mod entities;
pub use entities::{
    entity_candidates_from_evidence, entity_candidates_from_proposal, merge_entities,
    resolve_entities, split_entity, EntityProposal, EntityRecord,
};

mod dashboard;
pub use dashboard::{build_dashboard, render_dashboard_markdown, DashboardData};

mod ambient_distill;
pub use ambient_distill::{
    cluster_signals, distill_ambient_cluster, distill_ambient_cluster_pinned,
    distill_ambient_cluster_pinned_with_response, is_cluster_salient, stamp_ambient_evidence,
    AmbientDistillLlmOutcome, AmbientSignalRow, SignalCluster,
};

mod screen_distill;
pub use screen_distill::{
    cluster_screen_observations, distill_screen_cluster, distill_screen_cluster_source_bound,
    distill_screen_cluster_with_telemetry, is_screen_cluster_salient, stamp_screen_evidence,
    ScreenObservationCluster, ScreenObservationRow,
};

mod tier_distill;
pub use tier_distill::{
    cluster_tier_entries, distill_tier_cluster, is_tier_cluster_salient, producer_spec,
    stamp_tier_evidence, TierCluster, TierProducerSpec,
};
mod decision;

pub mod tier_contracts;
pub use tier_contracts::{
    tier_name_for, CALENDAR_EVIDENCE_TIER, CALENDAR_EVIDENCE_TIER_NAME, CHANNEL_FEEDBACK_TIER,
    CHANNEL_FEEDBACK_TIER_NAME, CHANNEL_PATTERNS_TIER, CHANNEL_PATTERNS_TIER_NAME,
    CHANNEL_WRITING_PREFERENCES_TIER, CHANNEL_WRITING_PREFERENCES_TIER_NAME, CHAT_EVIDENCE_TIER,
    CHAT_EVIDENCE_TIER_NAME, EMAIL_EVIDENCE_TIER, EMAIL_EVIDENCE_TIER_NAME,
    OBSERVED_EVIDENCE_AND_CHANNEL_TIERS, RESEARCH_FINDINGS_TIER, RESEARCH_FINDINGS_TIER_NAME,
    WORK_EVIDENCE_TIER,
};
mod tier_distill_run;
pub use tier_distill_run::{distill_tier_producer, distill_tier_producer_with_broadcaster};

pub mod eval;
pub mod observed_statements;
pub mod outward_assertions;
pub mod transcript_ingestion;

/// How the registers fold: indexed, and abandonable. It lives here rather than
/// inside either register for the same reason the journal below does — the
/// claims log and the commitment shards fold the same shape, and a cost fix
/// applied to one of them is a divergence rather than a fix.
pub mod store_cursor;
pub use store_cursor::{
    fold_was_cancelled, StoreFoldCancelled, StoreFoldCursor, StoreFoldIndex,
    FOLD_CANCELLATION_CHECK_RECORDS,
};

/// The lossless cursor over completed decisions. It lives beside the registers
/// rather than inside either one because it spans both: the claims log and the
/// per-audience commitment shards are two families of the same decision, and a
/// cursor that only covered one of them would not be a cursor over the scope.
pub mod completion_journal;
pub use completion_journal::{
    EvidenceCompletionCursor, EvidenceCompletionEntry, EvidenceCompletionJournal,
    EvidenceCompletionPage, EvidenceDecisionCompletion, EvidenceDecisionScope,
    EvidenceDecisionTarget, MAX_COMPLETION_PAGE_ENTRIES,
};

/// The journal's consumer: completed decisions become the package's
/// `review_receipt` rows. It lives here rather than in either register for the
/// same reason the journal does — one projection over both families, or the
/// commitment half of a decision would never reach the console that ordered it.
pub mod review_receipt_projection;
pub use review_receipt_projection::{
    PendingReviewReceipt, ProjectedReviewReceipt, ReviewReceiptDrain, ReviewReceiptOutcome,
    ReviewReceiptProjector, ReviewReceiptPublication, ReviewReceiptPublisher,
    ReviewReceiptTargetKind, MAX_REVIEW_RECEIPT_PAGE, MAX_REVIEW_RECEIPT_PUBLICATION_PAGE,
    REVIEW_RECEIPT_ENTITY, REVIEW_RECEIPT_UNNAMED_ACTOR,
};

mod compaction;
pub use compaction::{compact_evidence, CompactionOutcome};
/// What we told whom, through which exact payload. Owned here per the plan's §2:
/// consumers read, and never keep an authoritative copy of their own.
pub use observed_statements::{
    record_observed_statement, ClaimConfirmation, ExtractedClaim, ObservedStatement, PendingClaim,
    RecordedStatement,
};
pub use outward_assertions::{
    AffectedDisclosure, CorrectionObligation, ObligationState, OutwardActDisclosure,
    OutwardActStatus, OutwardActTransition, OutwardAssertionStore, OutwardAssertionUse,
    OutwardChannel, OutwardScope, PrepareOutwardAct, WorkAxisBackfill, PROVIDER_MESSAGE_AXIS,
};
/// The caller for the observed-channel writer, and the surface a person decides
/// a pending claim on. Without it the writer's *"an extraction is surfaced, not
/// dropped"* rule held only vacuously — nothing ingested a transcript.
pub use transcript_ingestion::{
    commitment_request_from_claim, ClaimDecisionDisposition, ClaimDecisionOutcome,
    ClaimDecisionReceipt, ClaimDecisionVerb, IngestedTranscript, OwnerDecision, SkipReason,
    SkippedUtterance, SpeakerAttribution, TranscriptClaim, TranscriptClaimStatus,
    TranscriptIngestion, TranscriptSource, TranscriptUtterance,
};

mod views;
pub use views::{cooccurrence_edges, entity_neighborhood, CooccurrenceEdge, EntityNeighborhood};

mod claims;
pub use claims::{
    parse_claims, propose_claims, validate_claim_grounding, ClaimGrounding, ClaimRecord,
    ClaimTimeWindow,
};

mod precision;
pub use precision::{
    grade_evidence_precision, grade_summary_precision, grade_summary_precision_with_telemetry,
    PrecisionReport, PrecisionVerdict, UnsupportedKind,
};

mod feedback;
pub use feedback::{utility, ReviewFeedback, ReviewFeedbackLedger, ReviewVerdict, UtilityReport};

mod worklog;
pub use worklog::render_agent_worklog_markdown;

/// Per-agent retention cap on persisted entity anchors (newest kept).
pub const ENTITY_RETENTION_CAP: usize = 2000;

/// Minimum importance for a promoted record to be *persisted*. Distillation
/// itself does not apply this gate (so validation can see everything the model
/// promotes); persistence call sites do, via [`is_salient`].
pub const EVIDENCE_SALIENCE_THRESHOLD: f64 = 0.4;

/// Per-agent retention cap on persisted evidence records (newest kept).
pub const EVIDENCE_RETENTION_CAP: usize = 1000;

/// User-driven lifecycle state for an evidence record. Durable across
/// re-distillation: a suppressed/deleted record is never silently resurrected
/// when its source episode is consolidated again (see
/// [`EvidenceRecord::is_user_corrected`] and the inbox correction flow).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    /// Live evidence — eligible for review synthesis and listed in the inbox.
    #[default]
    Active,
    /// Hidden from synthesis but kept (and shown in the inbox to un-suppress).
    Suppressed,
    /// Durable tombstone — excluded everywhere; kept so re-distillation of the
    /// source episode cannot bring it back.
    Deleted,
}

/// A derived, multi-label domain classification (open vocabulary).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Facet {
    /// Open-vocabulary domain label, e.g. `work`, `personal`, `business`.
    pub label: String,
    pub confidence: f64,
    /// Who assigned this facet: `llm` | `user` | `rule`.
    pub assigned_by: String,
}

/// Durable, prompt-worthy distilled evidence record.
///
/// Deterministic, system-owned fields (`evidence_id`, `source_refs`,
/// `first_seen_at` / `last_seen_at`, `sensitivity`) are stamped by the runtime;
/// the rest are LLM-proposed (see [`EvidenceProposal`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub evidence_id: String,
    pub summary: String,
    pub evidence_kind: String,
    #[serde(default)]
    pub observed_actions: Vec<String>,
    #[serde(default)]
    pub entity_keys: Vec<String>,
    #[serde(default)]
    pub people_keys: Vec<String>,
    #[serde(default)]
    pub artifact_refs: Vec<String>,
    pub source_refs: Vec<String>,
    #[serde(default)]
    pub facets: Vec<Facet>,
    pub importance: f64,
    pub confidence: f64,
    pub sensitivity: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
    /// Lifecycle state. Defaults to `active` so records written before this
    /// field existed deserialize as live.
    #[serde(default)]
    pub status: EvidenceStatus,
    /// RFC3339 timestamp of the last user correction (suppress / delete /
    /// re-facet). Reviews generated before this are stale w.r.t. this record.
    #[serde(default)]
    pub last_corrected_at: Option<String>,
    /// Which producer lane created this record — the self-describing discovery
    /// tag so consumers (reviews / dashboard) need not parse `source_refs`.
    /// `task_episode` (agent-scoped task distillation, default) | `ambient_browser`
    /// (user-owned normal-browsing distillation) | future `visual`, etc.
    #[serde(default = "default_producer")]
    pub producer: String,
    /// Open, extensible metadata bag for producer- or domain-specific fields that
    /// don't warrant a first-class column (e.g. work-ledger ticket refs). Defaults
    /// to JSON null so records written before this field existed still deserialize.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Default producer for records written before the field existed: the original
/// task-episode distillation lane.
pub fn default_producer() -> String {
    "task_episode".to_string()
}

impl EvidenceRecord {
    /// True when a human has touched this record — it is suppressed/deleted, or
    /// carries at least one user-assigned facet. Re-distillation must not
    /// overwrite such records (that would resurrect suppressed evidence or wipe
    /// a manual re-label).
    pub fn is_user_corrected(&self) -> bool {
        self.status != EvidenceStatus::Active || self.facets.iter().any(|f| f.assigned_by == "user")
    }

    /// Deterministic, run-grained id for a work-outcome ledger record. Each root
    /// execution is a distinct unit of work, so its evidence id keys off the root
    /// execution id (one ledger record per run) — never off the entity/day bucket
    /// that the ambient compaction pass merges on.
    pub fn work_outcome_id(root_execution_id: &str) -> String {
        format!("evd:run:{root_execution_id}")
    }

    /// Build a run-grained `work_outcome` ledger record from a completed run's
    /// summary-shaped inputs. Unlike the LLM-distilled lanes, this is a
    /// deterministic stamp: one record per root execution, `evidence_id` keyed off
    /// [`Self::work_outcome_id`], and a single system-assigned `work` facet.
    /// Producer/outcome-specific fields (outcome, open loops, next-step hint,
    /// task/agent ids) ride in the extensible [`metadata`](Self::metadata) bag.
    pub fn from_work_outcome(input: WorkOutcomeInput) -> Self {
        let stamped_at = DateTime::<Utc>::from_timestamp_millis(input.timestamp_ms)
            .unwrap_or_else(Utc::now)
            .to_rfc3339();
        Self {
            evidence_id: Self::work_outcome_id(&input.root_execution_id),
            summary: input.summary,
            evidence_kind: "work_outcome".to_string(),
            observed_actions: Vec::new(),
            entity_keys: input.entity_keys,
            people_keys: Vec::new(),
            artifact_refs: input.artifacts,
            source_refs: vec![format!("execution:{}", input.root_execution_id)],
            facets: vec![Facet {
                label: "work".to_string(),
                confidence: 1.0,
                assigned_by: "system".to_string(),
            }],
            importance: 0.5,
            confidence: 1.0,
            sensitivity: "work".to_string(),
            first_seen_at: stamped_at.clone(),
            last_seen_at: stamped_at,
            status: EvidenceStatus::Active,
            last_corrected_at: None,
            producer: "work_outcome".to_string(),
            metadata: serde_json::json!({
                "outcome": input.outcome,
                "open_loops": input.open_loops,
                "next_step_hint": input.next_step_hint,
                "task_id": input.task_id,
                "agent_id": input.agent_id,
            }),
        }
    }
}

/// Run-summary-shaped inputs for a deterministic `work_outcome` ledger record.
/// Populated by the work-ledger writer from a completed root execution's yield;
/// the fields that don't warrant a first-class [`EvidenceRecord`] column ride in
/// [`EvidenceRecord::metadata`] via [`EvidenceRecord::from_work_outcome`].
#[derive(Debug, Clone)]
pub struct WorkOutcomeInput {
    /// Root execution id — the unit of work; drives the run-grained evidence id.
    pub root_execution_id: String,
    pub task_id: Option<String>,
    pub agent_id: String,
    /// Terminal outcome: `success` | `failed` | `cannot_proceed` | `yield`.
    pub outcome: String,
    pub summary: String,
    pub artifacts: Vec<String>,
    pub open_loops: Vec<String>,
    pub next_step_hint: Option<String>,
    pub entity_keys: Vec<String>,
    /// When the run completed, epoch millis.
    pub timestamp_ms: i64,
}

/// What the distillation LLM proposes from a single episode. The runtime stamps
/// the deterministic fields when turning this into an [`EvidenceRecord`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvidenceProposal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_origin: Option<decision_engine_contract::classification::ClassificationOrigin>,
    #[serde(skip)]
    pub decision_guard: Option<EvidenceDecisionGuard>,
    /// Whether this episode is worth promoting as durable work evidence.
    pub promote: bool,
    /// One-line reason when `promote` is false (noise / trivial / no signal).
    #[serde(default)]
    pub skip_reason: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub evidence_kind: Option<String>,
    #[serde(default)]
    pub observed_actions: Vec<String>,
    #[serde(default)]
    pub entity_keys: Vec<String>,
    #[serde(default)]
    pub people_keys: Vec<String>,
    /// LLM-enriched entity anchors (canonical name + aliases + type) for the
    /// keys above. Optional — a model that only emits keys still resolves.
    #[serde(default)]
    pub entities: Vec<EntityProposal>,
    /// Proposed open-vocabulary domain facets (e.g. `work`).
    #[serde(default)]
    pub facets: Vec<FacetProposal>,
    #[serde(default)]
    pub importance: Option<f64>,
    #[serde(default)]
    pub confidence: Option<f64>,
    /// LLM-proposed sensitivity category — e.g. `work`, `personal`, `financial`,
    /// `health`, `credentials`, `private_comms`. Drives default suppression of
    /// sensitive evidence from consumers and from agent memory.
    #[serde(default)]
    pub sensitivity: Option<String>,
}

/// Runtime authority is never reconstructed from model JSON or a persisted proposal.
#[derive(Debug, Clone)]
pub struct EvidenceDecisionGuard(
    pub(crate) std::sync::Arc<crate::magician_v2::decisions::reference::ApplyGuard>,
);
impl EvidenceProposal {
    pub fn decision_is_current(&self) -> bool {
        self.decision_origin.is_none()
            || self
                .decision_guard
                .as_ref()
                .is_some_and(|guard| guard.0.current())
    }
}

/// An LLM-proposed facet label (confidence optional; defaulted on stamping).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FacetProposal {
    pub label: String,
    #[serde(default)]
    pub confidence: Option<f64>,
}

impl EvidenceRecord {
    /// Stamp an LLM proposal with the deterministic, system-owned fields drawn
    /// from its source episode. Returns `None` when the proposal opted out of
    /// promotion or lacks the minimum content to be useful.
    ///
    /// The `evidence_id` is derived from the episode id (one evidence per
    /// episode for now), so re-running distillation is idempotent rather than
    /// duplicating rows.
    pub fn from_proposal(proposal: &EvidenceProposal, episode: &V3EpisodeRecord) -> Option<Self> {
        if !proposal.promote || !proposal.decision_is_current() {
            return None;
        }
        let summary = proposal.summary.as_ref()?.trim().to_string();
        if summary.is_empty() {
            return None;
        }
        let facets = proposal
            .facets
            .iter()
            .filter(|f| !f.label.trim().is_empty())
            .map(|f| Facet {
                label: f.label.trim().to_lowercase(),
                confidence: f.confidence.unwrap_or(0.5).clamp(0.0, 1.0),
                assigned_by: "llm".to_string(),
            })
            .collect();
        Some(Self {
            evidence_id: format!("evd:{}", episode.episode_id),
            summary,
            evidence_kind: proposal
                .evidence_kind
                .clone()
                .filter(|k| !k.trim().is_empty())
                .unwrap_or_else(|| "activity".to_string()),
            observed_actions: proposal.observed_actions.clone(),
            entity_keys: proposal.entity_keys.clone(),
            people_keys: proposal.people_keys.clone(),
            artifact_refs: Vec::new(),
            source_refs: vec![format!("episode:{}", episode.episode_id)],
            facets,
            importance: proposal.importance.unwrap_or(0.5).clamp(0.0, 1.0),
            confidence: proposal.confidence.unwrap_or(0.5).clamp(0.0, 1.0),
            sensitivity: normalize_sensitivity(proposal.sensitivity.as_deref()),
            first_seen_at: episode.completed_at.clone(),
            last_seen_at: episode.completed_at.clone(),
            status: EvidenceStatus::Active,
            last_corrected_at: None,
            producer: default_producer(),
            metadata: proposal
                .decision_origin
                .as_ref()
                .map(|origin| serde_json::json!({"decision_origin": origin}))
                .unwrap_or(serde_json::Value::Null),
        })
    }
}

/// Evidence sensitivity categories that consumers suppress by default. Ambient
/// browsing inevitably captures private activity; only affirmatively-sensitive
/// categories are gated. `work` / `unknown` / unset flow normally (the memory
/// bridge stays review-gated as a backstop for unclassified evidence).
pub const SENSITIVE_CATEGORIES: &[&str] = &[
    "financial",
    "health",
    "medical",
    "credentials",
    "private_comms",
    "private",
    "personal",
    "legal",
    "adult",
];

/// Normalize an LLM-proposed sensitivity label: lowercased + trimmed; empty →
/// `unknown`.
pub fn normalize_sensitivity(raw: Option<&str>) -> String {
    let v = raw.unwrap_or_default().trim().to_lowercase();
    if v.is_empty() {
        "unknown".to_string()
    } else {
        v
    }
}

/// True when a record's sensitivity is an affirmatively-sensitive category that
/// default consumers suppress and the memory bridge refuses to promote into
/// agent-visible memory.
pub fn is_sensitive(sensitivity: &str) -> bool {
    SENSITIVE_CATEGORIES.contains(&sensitivity.trim().to_lowercase().as_str())
}

/// Distill a single episode into an evidence-record *proposal* (the raw LLM
/// output, including the promote/skip decision and reason). Callers stamp it
/// into an [`EvidenceRecord`] via [`EvidenceRecord::from_proposal`] and decide
/// whether to persist. Keeping the proposal lets validation surfaces show skip
/// reasons.
///
/// Shared by the `distill-evidence` CLI command and the consolidation auto-hook.
pub async fn distill_episode(
    episode: &V3EpisodeRecord,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
) -> anyhow::Result<EvidenceProposal> {
    Ok(
        distill_episode_with_response(episode, router, prompt_manager, None)
            .await?
            .proposal,
    )
}

pub struct EpisodeDistillLlmOutcome {
    pub proposal: EvidenceProposal,
    pub response: SimplifiedLLMResponse,
}

/// Distill an episode while preserving the router response for scoped token and
/// cost telemetry. Callers that already account elsewhere can keep using
/// [`distill_episode`].
pub async fn distill_episode_with_response(
    episode: &V3EpisodeRecord,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    replay_source: Option<(&AgentMemoryService, &Arc<PromptManager>)>,
) -> anyhow::Result<EpisodeDistillLlmOutcome> {
    use std::collections::HashMap;

    let system_prompt = prompt_manager
        .get_rendered_prompt("evidence_distill_system", "1.0.0", HashMap::new())
        .await?;
    let mut vars = HashMap::new();
    vars.insert(
        "episode_json".to_string(),
        serde_json::to_string_pretty(episode)?,
    );
    let user_prompt = prompt_manager
        .get_rendered_prompt("evidence_distill_user", "1.0.0", vars)
        .await?;

    let operation = LLMOperation::Other("distill_evidence".to_string());
    let reviewed = decision::review(
        router,
        operation.as_str(),
        &system_prompt,
        &user_prompt,
        false,
        replay_source.map(
            |(service, prompts)| decision::EvidenceReplaySource::Episode {
                service,
                episode,
                prompts,
            },
        ),
        || async {
            router
                .generate_for_chunkable_operation_with_system(
                    &operation,
                    Some(&system_prompt),
                    &user_prompt,
                    serde_json::json!({"episodes": [episode]}),
                    None,
                )
                .await
        },
    )
    .await?;
    Ok(EpisodeDistillLlmOutcome {
        proposal: reviewed.proposal,
        response: reviewed.response,
    })
}

/// Whether a distilled record clears the persistence salience gate.
pub fn is_salient(record: &EvidenceRecord) -> bool {
    record.importance >= EVIDENCE_SALIENCE_THRESHOLD
}

/// Tolerant JSON extraction — the model may wrap the object in fenced code
/// blocks or stray prose despite instructions.
pub fn parse_evidence_proposal(content: &str) -> anyhow::Result<EvidenceProposal> {
    let trimmed = content.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Ok(value);
    }
    let unfenced = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(value) = serde_json::from_str(unfenced) {
        return Ok(value);
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if end > start {
            return Ok(serde_json::from_str(&trimmed[start..=end])?);
        }
    }
    anyhow::bail!("could not parse an evidence proposal from the model response")
}

// ─── Read path: deterministic assembler + synthesis ─────────────────────────

/// Select evidence whose `last_seen_at` is at/after `since`, optionally filtered
/// to a facet label, most-recent first. Only `active` records are eligible —
/// suppressed and deleted records are excluded so corrections flow straight
/// through to the next synthesized review. Records with an unparseable timestamp
/// are kept (never silently dropped). Deterministic — the LLM does the grouping
/// and prose downstream.
pub fn select_evidence_window(
    records: &[EvidenceRecord],
    since: DateTime<Utc>,
    facet: Option<&str>,
) -> Vec<EvidenceRecord> {
    let mut selected: Vec<EvidenceRecord> = records
        .iter()
        .filter(|record| record.status == EvidenceStatus::Active)
        .filter(|record| {
            DateTime::parse_from_rfc3339(&record.last_seen_at)
                .map(|ts| ts.with_timezone(&Utc) >= since)
                .unwrap_or(true)
        })
        .filter(|record| {
            facet.map_or(true, |wanted| {
                record.facets.iter().any(|f| f.label == wanted)
            })
        })
        .cloned()
        .collect();
    selected.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
    selected
}

/// Build a compact, deterministic prompt packet from selected evidence and
/// return the input evidence ids (for output lineage / `input_evidence_ids`).
pub fn build_review_packet(records: &[EvidenceRecord]) -> (String, Vec<String>) {
    let mut packet = String::new();
    let mut ids = Vec::with_capacity(records.len());
    for record in records {
        ids.push(record.evidence_id.clone());
        let facets = if record.facets.is_empty() {
            "-".to_string()
        } else {
            record
                .facets
                .iter()
                .map(|f| f.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let entities = if record.entity_keys.is_empty() {
            "-".to_string()
        } else {
            record.entity_keys.join(", ")
        };
        let people = if record.people_keys.is_empty() {
            "-".to_string()
        } else {
            record.people_keys.join(", ")
        };
        packet.push_str(&format!(
            "- [{kind}] ({actions}) {summary}\n    facets: {facets} | entities: {entities} | people: {people} | when: {when} | id: {id}\n",
            kind = record.evidence_kind,
            actions = record.observed_actions.join("/"),
            summary = record.summary,
            when = record.last_seen_at,
            id = record.evidence_id,
        ));
    }
    (packet, ids)
}

/// Synthesize an impact summary (Markdown) over the assembled evidence packet
/// for a caller-chosen window — there is no fixed weekly cadence; a UI slider
/// binds to `window_days`. Grounding/structure rules live in the
/// `evidence_review` store prompt.
pub async fn synthesize_review(
    packet: &str,
    window_days: i64,
    facet: Option<&str>,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
) -> anyhow::Result<String> {
    synthesize_review_with_telemetry(packet, window_days, facet, router, prompt_manager, None).await
}

pub async fn synthesize_review_with_telemetry(
    packet: &str,
    window_days: i64,
    facet: Option<&str>,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    telemetry: Option<
        &crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext,
    >,
) -> anyhow::Result<String> {
    use std::collections::HashMap;

    let mut vars = HashMap::new();
    vars.insert("window_days".to_string(), window_days.to_string());
    vars.insert("facet".to_string(), facet.unwrap_or("all").to_string());
    vars.insert("evidence_packet".to_string(), packet.to_string());
    let prompt = prompt_manager
        .get_rendered_prompt("evidence_review", "1.0.0", vars)
        .await?;

    let operation = LLMOperation::Other("evidence_review".to_string());
    let llm_started = std::time::Instant::now();
    let response = router
        .generate_for_operation_with_system(&operation, None, &prompt)
        .await?;
    let content = response.content.trim().to_string();
    if let Some(telemetry) = telemetry {
        if content.is_empty() {
            telemetry.emit_validation_failure(
                "evidence_review",
                &response,
                llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                Default::default(),
                "evidence_review_nonempty",
                "review was empty",
            );
        } else {
            telemetry.emit_validated_success(
                "evidence_review",
                &response,
                llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                Default::default(),
                "evidence_review_nonempty",
            );
        }
    }
    Ok(content)
}

// ─── Verification gate (Slice 5) ────────────────────────────────────────────

/// Grounding verdict for a synthesized review: a deterministic citation-coverage
/// check (every claim bullet must cite an admissible `evd:` id) combined with an
/// LLM critic that catches invented/exaggerated claims that cite real ids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    /// True only when no claim bullet is uncited AND the critic found no
    /// unsupported claim. The output should be flagged/blocked when false.
    pub grounded: bool,
    /// Claims the critic judged unsupported by the evidence.
    pub ungrounded_claims: Vec<String>,
    /// Fraction of claim bullets carrying at least one admissible citation.
    pub citation_coverage: f64,
    /// Claim bullets that carried no admissible citation.
    pub uncited_bullets: usize,
    pub total_bullets: usize,
    /// Distinct `evd:` ids actually cited in the review text.
    pub cited_ids: Vec<String>,
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ReviewVerifyOutput {
    #[serde(default)]
    grounded: bool,
    #[serde(default)]
    ungrounded_claims: Vec<String>,
    #[serde(default)]
    notes: String,
}

/// Extract every `evd:<id>` token cited in free text (e.g. `[evd:ep_991]`),
/// deduped. ASCII-only matching keeps byte slicing safe.
pub fn extract_cited_ids(text: &str) -> Vec<String> {
    const PAT: &[u8] = b"evd:";
    let bytes = text.as_bytes();
    let mut ids = Vec::new();
    let mut i = 0;
    while i + PAT.len() <= bytes.len() {
        if &bytes[i..i + PAT.len()] == PAT {
            let start = i;
            let mut j = i + PAT.len();
            while j < bytes.len() {
                let c = bytes[j];
                if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.' {
                    j += 1;
                } else {
                    break;
                }
            }
            if j > start + PAT.len() {
                ids.push(text[start..j].to_string());
            }
            i = j.max(start + 1);
        } else {
            i += 1;
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Deterministic grounding signal: fraction of claim bullets (lines starting
/// with `-`/`*`) carrying at least one citation to an *admissible* id (one in
/// `input_ids`). Returns `(coverage, grounded_bullets, total_bullets)`; a review
/// with no bullets scores 1.0 (nothing to ground).
pub fn review_citation_coverage(review_md: &str, input_ids: &[String]) -> (f64, usize, usize) {
    use std::collections::HashSet;
    let admissible: HashSet<&str> = input_ids.iter().map(String::as_str).collect();
    let mut total = 0usize;
    let mut grounded = 0usize;
    for line in review_md.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            total += 1;
            if extract_cited_ids(line)
                .iter()
                .any(|id| admissible.contains(id.as_str()))
            {
                grounded += 1;
            }
        }
    }
    let coverage = if total == 0 {
        1.0
    } else {
        grounded as f64 / total as f64
    };
    (coverage, grounded, total)
}

/// Verify a synthesized review against its evidence packet. Combines the
/// deterministic citation check with the `evidence_review_verify` critic op; the
/// review is `grounded` only if no bullet is uncited and the critic flags nothing.
pub async fn verify_review(
    review_md: &str,
    packet: &str,
    input_ids: &[String],
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
) -> anyhow::Result<VerificationReport> {
    verify_review_with_telemetry(review_md, packet, input_ids, router, prompt_manager, None).await
}

pub async fn verify_review_with_telemetry(
    review_md: &str,
    packet: &str,
    input_ids: &[String],
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    telemetry: Option<
        &crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext,
    >,
) -> anyhow::Result<VerificationReport> {
    use std::collections::HashMap;

    let (coverage, grounded_bullets, total_bullets) =
        review_citation_coverage(review_md, input_ids);
    let cited_ids = extract_cited_ids(review_md);

    let mut vars = HashMap::new();
    vars.insert("evidence_packet".to_string(), packet.to_string());
    vars.insert("review_markdown".to_string(), review_md.to_string());
    let prompt = prompt_manager
        .get_rendered_prompt("evidence_review_verify", "1.0.0", vars)
        .await?;
    let operation = LLMOperation::Other("evidence_review_verify".to_string());
    let llm_started = std::time::Instant::now();
    let response = router
        .generate_for_operation_with_system(&operation, None, &prompt)
        .await?;
    // A verifier that returns garbage must not silently pass a bad review, but
    // also must not block on its own failure: fall back to the deterministic
    // signal only.
    let parsed = match parse_review_verify(&response.content) {
        Ok(parsed) => {
            if let Some(telemetry) = telemetry {
                telemetry.emit_validated_success(
                    "evidence_review_verify",
                    &response,
                    llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    Default::default(),
                    "evidence_review_verdict",
                );
            }
            parsed
        },
        Err(_) => {
            if let Some(telemetry) = telemetry {
                telemetry.emit_validation_failure(
                    "evidence_review_verify",
                    &response,
                    llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    Default::default(),
                    "evidence_review_verdict",
                    "verifier output was not parseable",
                );
            }
            ReviewVerifyOutput {
                grounded: total_bullets == 0 || grounded_bullets == total_bullets,
                ungrounded_claims: Vec::new(),
                notes: "verifier output unparseable; deterministic citation check only".to_string(),
            }
        },
    };

    let uncited_bullets = total_bullets.saturating_sub(grounded_bullets);
    Ok(VerificationReport {
        grounded: parsed.grounded && uncited_bullets == 0,
        ungrounded_claims: parsed.ungrounded_claims,
        citation_coverage: coverage,
        uncited_bullets,
        total_bullets,
        cited_ids,
        notes: parsed.notes,
    })
}

fn parse_review_verify(content: &str) -> anyhow::Result<ReviewVerifyOutput> {
    let trimmed = content.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Ok(value);
    }
    let unfenced = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(value) = serde_json::from_str(unfenced) {
        return Ok(value);
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if end > start {
            return Ok(serde_json::from_str(&trimmed[start..=end])?);
        }
    }
    anyhow::bail!("could not parse a review verification result")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn sample_record() -> EvidenceRecord {
        EvidenceRecord {
            evidence_id: "evd:test".into(),
            summary: "s".into(),
            evidence_kind: "activity".into(),
            observed_actions: vec![],
            entity_keys: vec![],
            people_keys: vec![],
            artifact_refs: vec![],
            source_refs: vec![],
            facets: vec![],
            importance: 0.6,
            confidence: 0.6,
            sensitivity: "unknown".into(),
            first_seen_at: "2026-06-13T00:00:00Z".into(),
            last_seen_at: "2026-06-13T00:00:00Z".into(),
            status: EvidenceStatus::Active,
            last_corrected_at: None,
            producer: "task_episode".into(),
            metadata: serde_json::json!({ "ticket": "ABC-1", "hours": 2 }),
        }
    }

    #[test]
    fn metadata_bag_round_trips_through_serde() {
        let record = sample_record();
        let json = serde_json::to_string(&record).expect("serialize");
        let restored: EvidenceRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            record.metadata, restored.metadata,
            "metadata must survive a serialize -> deserialize round-trip"
        );
    }

    #[test]
    fn legacy_record_without_metadata_key_still_deserializes() {
        // A record written before `metadata` existed carries no such key. It must
        // deserialize (defaulting the bag), not error.
        let legacy = r#"{
            "evidence_id": "evd:legacy",
            "summary": "s",
            "evidence_kind": "activity",
            "source_refs": [],
            "importance": 0.6,
            "confidence": 0.6,
            "sensitivity": "unknown",
            "first_seen_at": "2026-06-13T00:00:00Z",
            "last_seen_at": "2026-06-13T00:00:00Z"
        }"#;
        let record: EvidenceRecord =
            serde_json::from_str(legacy).expect("legacy record without metadata must deserialize");
        assert!(
            record.metadata.is_null(),
            "absent metadata should default to JSON null"
        );
    }

    #[test]
    fn work_outcome_id_is_run_grained() {
        assert_eq!(
            EvidenceRecord::work_outcome_id("exec-root"),
            "evd:run:exec-root",
            "work-outcome ledger id is derived per root execution run"
        );
    }

    #[test]
    fn from_work_outcome_builds_a_run_grained_ledger_record() {
        let input = WorkOutcomeInput {
            root_execution_id: "exec-root".into(),
            task_id: Some("task-1".into()),
            agent_id: "agent-a".into(),
            outcome: "success".into(),
            summary: "Shipped the feature".into(),
            artifacts: vec!["artifact:report.md".into(), "artifact:pr-42".into()],
            open_loops: vec!["follow up on review".into()],
            next_step_hint: Some("merge after CI".into()),
            entity_keys: vec!["repo:magician".into()],
            timestamp_ms: 1_760_000_000_000,
        };
        let record = EvidenceRecord::from_work_outcome(input);

        assert_eq!(record.producer, "work_outcome");
        assert_eq!(record.evidence_kind, "work_outcome");
        assert_eq!(record.evidence_id, "evd:run:exec-root");
        assert!(
            record.facets.iter().any(|f| f.label == "work"),
            "a `work` facet must be present"
        );
        assert_eq!(
            record.artifact_refs,
            vec![
                "artifact:report.md".to_string(),
                "artifact:pr-42".to_string()
            ]
        );
        assert_eq!(record.entity_keys, vec!["repo:magician".to_string()]);
        assert_eq!(record.source_refs, vec!["execution:exec-root".to_string()]);
        assert_eq!(record.metadata["outcome"], "success");
        assert_eq!(record.metadata["open_loops"][0], "follow up on review");
        assert_eq!(record.metadata["next_step_hint"], "merge after CI");
        assert_eq!(record.metadata["task_id"], "task-1");
        assert_eq!(record.metadata["agent_id"], "agent-a");
    }

    #[test]
    fn work_outcome_producer_is_registered() {
        let spec = super::producer_spec("work_outcome")
            .expect("work_outcome must be a recognized producer");
        assert_eq!(spec.producer, "work_outcome");
    }
}
