//! Scoped durable subscriptions and deterministic Observe polling.
//!
//! The runner executes exact profile actions. It deliberately does not use the
//! progressive retrieval ladder: an RSS-only subscription can therefore never
//! fall back to search, browser, CDP, or an agent-selected tool.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use futures_util::stream::{self, StreamExt};
use magician_vector_index::OllamaEmbedder;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex as AsyncMutex, Semaphore},
    time::timeout,
};
use tracing::instrument;
use uuid::Uuid;

use super::{
    canonicalize_http_url, discover_observable_sources, project_observable_source_offers,
    public_http::validate_public_http_url, types::validate_content_scope_component, AdapterCost,
    ContentAcquisitionResolver, ContentAcquisitionService, ContentCandidate,
    ContentInvocationSource, ContentPrivacy, ContentProvenance, DiscoveryPage, DiscoveryRequest,
    DiscoveryTransportStats, DiscoveryValidator, FreshnessPolicy, ObservableActionBinding,
    ObservableCatalog, ObservableCatalogIssue, ObservableSourceOffer, ObservableSourceReadiness,
    ObservableSourceSettings, ObservableUnavailableReason, ObservationCadence,
    ObservationProfileLimits, RemoteDataPolicy, SourceIdentity, CONTENT_SOURCE_SCHEMA_VERSION,
};
// Share the resurfacing centrality cosine/normalize helpers so the semantic
// intent filter scores vectors identically to the resurfacing embedding path.
use crate::magician_v2::resurfacing_seam::{
    candidate_id, cosine, normalize, Candidate as ResurfacingCandidate, CandidateState,
    ResurfacingContentDetails, ResurfacingDetailStatus, ResurfacingSink, ResurfacingTemporalFact,
    ResurfacingWakeHandle, SalienceSignals, SourceKind,
};
use crate::magician_v2::{
    analytics::runtime_activity_layer::{KIND_BACKGROUND, WORKLOAD_AMBIENT},
    artifact_v2::workspace::ArtifactV2Workspace,
    execution::compiled_handlers::web_fetch::strip_html_to_text,
    notes::NotesSettingsStore,
    observe_catchup::{CatchUpDecision, CatchUpReplayMode, ObserveCatchUpController},
};

const SUBSCRIPTION_SCHEMA_VERSION: u32 = 1;
const SUBSCRIPTIONS_FILE: &str = "subscriptions.json";
const OBSERVABILITY_FILE: &str = "observability.json";
const OBSERVATION_ROOT: &str = "observe_sources";
const ENRICHMENT_INGRESS_DIR: &str = "enrichment_ingress";
const ENRICHMENT_FAILED_DIR: &str = "enrichment_failed";
const MAX_ENRICHMENT_BATCH_PER_SCOPE: usize = 100;
const MAX_FAILED_HANDOFFS_PER_SCOPE: usize = 256;
const MAX_SUBSCRIPTIONS_PER_SCOPE: usize = 2_048;
const MAX_SEEN_FINGERPRINTS: usize = 4_096;
const MAX_INTENT_CHARS: usize = 1_024;
const MAX_CUSTOM_NAME_CHARS: usize = 160;
const MAX_PAGE_SIZE: usize = 100;
const MAX_OBSERVATION_RUN_HISTORY: usize = 4_096;
const MAX_OBSERVATION_SOURCE_AGGREGATES: usize = 4_096;
const RUN_NOW_COOLDOWN_MS: i64 = 30_000;
const NOTES_SOURCE_ID: &str = "notes";
const NOTES_PROFILE_ID: &str = "observe-notes";
const NOTES_ACTION_ID: &str = "notes.discover";
const NOTES_ADAPTER_ID: &str = "notes";
const NOTES_TARGET: &str = "notes://configured";
const NOTES_MAX_CURSOR_OFFSET: usize = 20_000;

/// Minimum clamped cosine similarity for a candidate to be KEPT on the semantic
/// side of the hybrid interest filter (when it shares no keyword with the
/// intent). Overridable at runtime via `OBSERVE_INTENT_SEMANTIC_MIN`. Chosen
/// conservatively so only genuinely on-topic items survive semantic-only;
/// keyword matches are always kept regardless of this floor.
const SEMANTIC_INTENT_MIN: f32 = 0.55;

/// Resolve the semantic keep-floor, honoring the `OBSERVE_INTENT_SEMANTIC_MIN`
/// override (falls back to [`SEMANTIC_INTENT_MIN`] on unset/unparsable).
fn semantic_intent_min() -> f32 {
    std::env::var("OBSERVE_INTENT_SEMANTIC_MIN")
        .ok()
        .and_then(|value| value.trim().parse::<f32>().ok())
        .unwrap_or(SEMANTIC_INTENT_MIN)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSubscriptionState {
    Enabled,
    Paused,
    Backoff,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationSubscription {
    pub schema_version: u32,
    pub subscription_id: String,
    pub principal: String,
    pub workspace: String,
    pub source_id: String,
    pub source_revision: String,
    pub profile_id: String,
    pub profile_revision: String,
    pub display_name: String,
    pub category: String,
    pub enabled: bool,
    pub custom: bool,
    pub cadence: ObservationCadence,
    #[serde(default = "default_supported_cadences")]
    pub supported_cadence: Vec<ObservationCadence>,
    pub next_run_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    pub max_candidates_per_run: usize,
    pub max_selected_per_run: usize,
    pub action_bindings: Vec<ObservableActionBinding>,
    pub targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub validators: BTreeMap<String, DiscoveryValidator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_observed_identity: Option<String>,
    #[serde(default)]
    pub seen_fingerprints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at_ms: Option<i64>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_class: Option<String>,
    #[serde(default, alias = "lease_id", skip_serializing_if = "Option::is_none")]
    pub lease_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_expires_at_ms: Option<i64>,
    pub revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl ObservationSubscription {
    pub fn state(&self, now_ms: i64) -> ObservationSubscriptionState {
        if !self.enabled {
            ObservationSubscriptionState::Paused
        } else if self.consecutive_failures > 0 && self.next_run_at_ms > now_ms {
            ObservationSubscriptionState::Backoff
        } else {
            ObservationSubscriptionState::Enabled
        }
    }

    fn validate_persisted(&self, principal: &str, workspace: &str) -> Result<()> {
        if self.schema_version != SUBSCRIPTION_SCHEMA_VERSION {
            anyhow::bail!("unsupported observable source subscription schema");
        }
        if self.principal != principal || self.workspace != workspace {
            anyhow::bail!("observable source subscription escaped its persisted scope");
        }
        validate_content_scope_component(&self.principal, "observable source principal")?;
        validate_content_scope_component(&self.workspace, "observable source workspace")?;
        validate_content_scope_component(&self.source_id, "observable source id")?;
        validate_content_scope_component(&self.profile_id, "observable source profile id")?;
        if self.subscription_id
            != subscription_id(
                principal,
                workspace,
                &self.source_id,
                &self.profile_id,
                self.targets.first().map(String::as_str),
            )
        {
            anyhow::bail!("observable source subscription identity is inconsistent");
        }
        if self.display_name.trim().is_empty()
            || self.display_name.chars().count() > MAX_CUSTOM_NAME_CHARS
            || self.display_name.chars().any(char::is_control)
            || self.category.trim().is_empty()
            || self.category.chars().count() > 64
            || self.category.chars().any(char::is_control)
        {
            anyhow::bail!("observable source subscription display metadata is invalid");
        }
        if self.supported_cadence.is_empty()
            || self.supported_cadence.len() > 3
            || !self.supported_cadence.contains(&self.cadence)
            || self
                .supported_cadence
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != self.supported_cadence.len()
        {
            anyhow::bail!("observable source subscription cadence is invalid");
        }
        if self.intent.as_deref().is_some_and(|value| {
            value.trim().is_empty()
                || value.chars().count() > MAX_INTENT_CHARS
                || value.chars().any(char::is_control)
        }) {
            anyhow::bail!("observable source subscription intent is invalid");
        }
        if self.max_candidates_per_run == 0
            || self.max_candidates_per_run > 200
            || self.max_selected_per_run == 0
            || self.max_selected_per_run > 50
            || self.max_selected_per_run > self.max_candidates_per_run
        {
            anyhow::bail!("observable source subscription limits are invalid");
        }
        if self.action_bindings.len() != 1
            || self.action_bindings.iter().any(|binding| {
                binding.action_id.trim().is_empty()
                    || binding.action_id.chars().count() > 128
                    || binding.action_id.chars().any(char::is_control)
                    || binding.adapter_id.trim().is_empty()
                    || binding.adapter_id.chars().count() > 128
                    || binding.adapter_id.chars().any(char::is_control)
            })
        {
            anyhow::bail!("observable source subscription action binding is invalid");
        }
        let notes_target = is_notes_binding(self.action_bindings.first());
        if self.targets.is_empty()
            || self.targets.len() > 16
            || if notes_target {
                self.source_id != NOTES_SOURCE_ID
                    || self.profile_id != NOTES_PROFILE_ID
                    || self.targets.len() != 1
                    || self.targets.first().map(String::as_str) != Some(NOTES_TARGET)
            } else {
                self.targets.iter().any(|target| {
                    target.chars().count() > 8 * 1024 || validate_public_http_url(target).is_err()
                })
            }
        {
            anyhow::bail!("observable source subscription targets are invalid");
        }
        if self.cursor.as_deref().is_some_and(|cursor| {
            cursor.is_empty()
                || cursor.chars().count() > 8 * 1024
                || cursor.chars().any(char::is_control)
        }) || self.validators.len() > self.targets.len()
            || self.validators.iter().any(|(target, validator)| {
                !self.targets.contains(target) || validator.validate().is_err()
            })
        {
            anyhow::bail!("observable source subscription checkpoint is invalid");
        }
        if self.seen_fingerprints.len() > MAX_SEEN_FINGERPRINTS
            || self.seen_fingerprints.iter().any(|fingerprint| {
                fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            anyhow::bail!("observable source subscription dedup state is invalid");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionFile {
    #[serde(default = "subscription_schema_version")]
    schema_version: u32,
    #[serde(default)]
    subscriptions: Vec<ObservationSubscription>,
}

impl Default for SubscriptionFile {
    fn default() -> Self {
        Self {
            schema_version: SUBSCRIPTION_SCHEMA_VERSION,
            subscriptions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutObservationSubscription {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub cadence: ObservationCadence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_candidates_per_run: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_selected_per_run: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_rss: Option<CustomRssSubscription>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomRssSubscription {
    pub display_name: String,
    pub feed_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservableSourceOfferPage {
    pub items: Vec<ObservableSourceOffer>,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub catalog_revision: String,
    pub manifest_issues: Vec<ObservableCatalogIssue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservationSubscriptionPage {
    pub items: Vec<ObservationSubscription>,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservationSubscriptionView {
    pub subscription_id: String,
    pub source_id: String,
    pub source_revision: String,
    pub profile_id: String,
    pub profile_revision: String,
    pub display_name: String,
    pub category: String,
    pub enabled: bool,
    pub custom: bool,
    pub action_id: String,
    pub cadence: ObservationCadence,
    pub supported_cadence: Vec<ObservationCadence>,
    pub next_run_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    pub max_candidates_per_run: usize,
    pub max_selected_per_run: usize,
    pub targets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_started_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success_at_ms: Option<i64>,
    pub consecutive_failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_class: Option<String>,
    pub revision: u64,
}

impl From<ObservationSubscription> for ObservationSubscriptionView {
    fn from(subscription: ObservationSubscription) -> Self {
        Self {
            subscription_id: subscription.subscription_id,
            source_id: subscription.source_id,
            source_revision: subscription.source_revision,
            profile_id: subscription.profile_id,
            profile_revision: subscription.profile_revision,
            display_name: subscription.display_name,
            category: subscription.category,
            enabled: subscription.enabled,
            custom: subscription.custom,
            action_id: subscription
                .action_bindings
                .first()
                .map(|binding| binding.action_id.clone())
                .unwrap_or_default(),
            cadence: subscription.cadence,
            supported_cadence: subscription.supported_cadence,
            next_run_at_ms: subscription.next_run_at_ms,
            intent: subscription.intent,
            max_candidates_per_run: subscription.max_candidates_per_run,
            max_selected_per_run: subscription.max_selected_per_run,
            targets: subscription.targets,
            last_run_started_at_ms: subscription.last_run_started_at_ms,
            last_success_at_ms: subscription.last_success_at_ms,
            consecutive_failures: subscription.consecutive_failures,
            last_error_class: subscription.last_error_class,
            revision: subscription.revision,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ObservableSourceMetricsSnapshot {
    pub offer_projections: u64,
    pub offers_eligible: u64,
    pub offers_needs_setup: u64,
    pub offers_unavailable: u64,
    pub runs_started: u64,
    pub runs_succeeded: u64,
    pub runs_failed: u64,
    pub runs_throttled: u64,
    pub leases_skipped: u64,
    pub policy_denials: u64,
    pub candidates_discovered: u64,
    pub candidates_deduped: u64,
    pub candidates_selected: u64,
    pub enrichment_handoffs: u64,
    pub enrichment_processed: u64,
    pub enrichment_failed: u64,
    pub cursor_advances: u64,
    pub modified_targets: u64,
    pub not_modified_targets: u64,
    pub response_bytes: u64,
    pub cost_microunits: BTreeMap<String, u64>,
    pub total_latency_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationRunTrigger {
    Scheduled,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationRunStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationRunRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub subscription_id: String,
    pub source_id: String,
    pub profile_id: String,
    pub action_id: String,
    pub trigger: ObservationRunTrigger,
    pub status: ObservationRunStatus,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub duration_ms: u64,
    pub discovered: u64,
    pub deduped: u64,
    pub selected: u64,
    pub handed_off: u64,
    pub cursor_advanced: bool,
    pub modified_targets: u64,
    pub not_modified_targets: u64,
    pub response_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<AdapterCost>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservationSourceAggregate {
    #[serde(default)]
    runs: u64,
    #[serde(default)]
    succeeded: u64,
    #[serde(default)]
    failed: u64,
    #[serde(default)]
    cancelled: u64,
    #[serde(default)]
    candidates_discovered: u64,
    #[serde(default)]
    candidates_deduped: u64,
    #[serde(default)]
    candidates_selected: u64,
    #[serde(default)]
    enrichment_handoffs: u64,
    #[serde(default)]
    enrichment_processed: u64,
    #[serde(default)]
    enrichment_failed: u64,
    #[serde(default)]
    cursor_advances: u64,
    #[serde(default)]
    modified_targets: u64,
    #[serde(default)]
    not_modified_targets: u64,
    #[serde(default)]
    response_bytes: u64,
    #[serde(default)]
    total_latency_ms: u64,
    #[serde(default)]
    cost_microunits: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_run: Option<ObservationRunRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservationObservabilityFile {
    #[serde(default = "subscription_schema_version")]
    schema_version: u32,
    #[serde(default)]
    sources: BTreeMap<String, ObservationSourceAggregate>,
    #[serde(default)]
    recent_runs: Vec<ObservationRunRecord>,
}

impl Default for ObservationObservabilityFile {
    fn default() -> Self {
        Self {
            schema_version: SUBSCRIPTION_SCHEMA_VERSION,
            sources: BTreeMap::new(),
            recent_runs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ObservationObservabilityTotals {
    pub subscriptions: u64,
    pub enabled: u64,
    pub healthy: u64,
    pub degraded: u64,
    pub never_run: u64,
    pub runs: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub candidates_discovered: u64,
    pub candidates_deduped: u64,
    pub candidates_selected: u64,
    pub enrichment_handoffs: u64,
    pub enrichment_processed: u64,
    pub enrichment_failed: u64,
    pub modified_targets: u64,
    pub not_modified_targets: u64,
    pub response_bytes: u64,
    pub total_latency_ms: u64,
    pub cost_microunits: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservationSourceObservabilitySummary {
    pub subscription_id: String,
    pub source_id: String,
    pub profile_id: String,
    pub display_name: String,
    pub category: String,
    pub action_id: String,
    pub enabled: bool,
    pub cadence: ObservationCadence,
    pub next_run_at_ms: i64,
    pub consecutive_failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_class: Option<String>,
    pub runs: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub candidates_discovered: u64,
    pub candidates_deduped: u64,
    pub candidates_selected: u64,
    pub enrichment_handoffs: u64,
    pub enrichment_processed: u64,
    pub enrichment_failed: u64,
    pub cursor_advances: u64,
    pub modified_targets: u64,
    pub not_modified_targets: u64,
    pub response_bytes: u64,
    pub total_latency_ms: u64,
    pub cost_microunits: BTreeMap<String, u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<ObservationRunRecord>,
}

impl ObservationSourceObservabilitySummary {
    fn from_subscription(
        subscription: ObservationSubscription,
        aggregate: ObservationSourceAggregate,
    ) -> Self {
        Self {
            subscription_id: subscription.subscription_id,
            source_id: subscription.source_id,
            profile_id: subscription.profile_id,
            display_name: subscription.display_name,
            category: subscription.category,
            action_id: subscription
                .action_bindings
                .first()
                .map(|binding| binding.action_id.clone())
                .unwrap_or_default(),
            enabled: subscription.enabled,
            cadence: subscription.cadence,
            next_run_at_ms: subscription.next_run_at_ms,
            consecutive_failures: subscription.consecutive_failures,
            last_error_class: subscription.last_error_class,
            runs: aggregate.runs,
            succeeded: aggregate.succeeded,
            failed: aggregate.failed,
            cancelled: aggregate.cancelled,
            candidates_discovered: aggregate.candidates_discovered,
            candidates_deduped: aggregate.candidates_deduped,
            candidates_selected: aggregate.candidates_selected,
            enrichment_handoffs: aggregate.enrichment_handoffs,
            enrichment_processed: aggregate.enrichment_processed,
            enrichment_failed: aggregate.enrichment_failed,
            cursor_advances: aggregate.cursor_advances,
            modified_targets: aggregate.modified_targets,
            not_modified_targets: aggregate.not_modified_targets,
            response_bytes: aggregate.response_bytes,
            total_latency_ms: aggregate.total_latency_ms,
            cost_microunits: aggregate.cost_microunits,
            last_run: aggregate.last_run,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservationSourceObservabilityPage {
    pub items: Vec<ObservationSourceObservabilitySummary>,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub totals: ObservationObservabilityTotals,
    pub handoff_backlog: usize,
    pub failed_handoffs: usize,
    pub run_history_retained: usize,
    pub runtime_metrics: ObservableSourceMetricsSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservationRunRecordPage {
    pub items: Vec<ObservationRunRecord>,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ObservationRunOutcome {
    pub subscription_id: String,
    pub action_id: String,
    pub discovered: usize,
    pub deduped: usize,
    pub selected: usize,
    pub handed_off: usize,
    pub cursor_advanced: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<AdapterCost>,
    #[serde(flatten)]
    pub transport: DiscoveryTransportStats,
    #[serde(skip_serializing)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SubscriptionMutationError {
    #[error("observable source subscription was not found")]
    NotFound,
    #[error("observable source profile is unavailable")]
    Unavailable,
    #[error("observable source definition changed; refresh and retry")]
    StaleSource,
    #[error("observable source subscription changed; refresh and retry")]
    RevisionConflict,
    #[error("observable source page changed; restart pagination")]
    StaleCursor,
    #[error("observable source subscription is invalid: {0}")]
    Invalid(String),
    #[error("observable source run is already active or was requested too recently")]
    Busy,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserveMatchReceipt {
    pub schema_version: u32,
    pub receipt_id: String,
    pub subscription_id: String,
    pub source_id: String,
    pub profile_id: String,
    pub action_id: String,
    pub candidate_fingerprint: String,
    pub selected_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevance_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedContentEnvelope {
    pub schema_version: u32,
    pub principal: String,
    pub workspace: String,
    pub source_id: String,
    pub source_revision: String,
    pub profile_id: String,
    pub profile_revision: String,
    pub subscription_id: String,
    pub action_id: String,
    pub candidate: ContentCandidate,
    pub selection: ObserveMatchReceipt,
    pub admitted_at_ms: i64,
}

pub struct ObservableSourceRuntime {
    workspace: ArtifactV2Workspace,
    notes_store: NotesSettingsStore,
    resolver: Arc<dyn ObservableContentResolver>,
    settings: ObservableSourceSettings,
    scope_locks: Mutex<HashMap<(String, String), Arc<AsyncMutex<()>>>>,
    run_slots: Semaphore,
    metrics: Mutex<ObservableSourceMetricsSnapshot>,
    runtime_id: String,
    resurfacing_sink: Option<Arc<dyn ResurfacingSink>>,
    resurfacing_wake: ResurfacingWakeHandle,
    /// Semantic side of the hybrid interest filter. Defaults to the
    /// keyword-only no-op ([`NoSemanticIntent`]); the bin wires an
    /// embedding-backed scorer.
    intent_scorer: Arc<dyn SemanticIntentScorer>,
    enrichment_lock: AsyncMutex<()>,
    startup_catch_up: Option<Arc<ObserveCatchUpController>>,
}

#[async_trait]
trait ObservableContentResolver: Send + Sync {
    async fn resolve(
        &self,
        principal: String,
        workspace: String,
    ) -> Result<Arc<ContentAcquisitionService>>;
}

struct ProductionObservableContentResolver {
    resolver: Arc<ContentAcquisitionResolver>,
}

#[async_trait]
impl ObservableContentResolver for ProductionObservableContentResolver {
    async fn resolve(
        &self,
        principal: String,
        workspace: String,
    ) -> Result<Arc<ContentAcquisitionService>> {
        self.resolver.resolve(principal, workspace).await
    }
}

impl std::fmt::Debug for ObservableSourceRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObservableSourceRuntime")
            .field("settings", &self.settings)
            .field("runtime_id", &self.runtime_id)
            .finish_non_exhaustive()
    }
}

impl ObservableSourceRuntime {
    pub fn new(
        workspace: ArtifactV2Workspace,
        resolver: Arc<ContentAcquisitionResolver>,
        settings: ObservableSourceSettings,
    ) -> Result<Self> {
        settings.validate_bounds()?;
        let run_slots = Semaphore::new(settings.max_concurrency);
        let notes_store = NotesSettingsStore::with_workspace_layout(workspace.clone());
        Ok(Self {
            workspace,
            notes_store,
            resolver: Arc::new(ProductionObservableContentResolver { resolver }),
            settings,
            scope_locks: Mutex::new(HashMap::new()),
            run_slots,
            metrics: Mutex::new(ObservableSourceMetricsSnapshot::default()),
            runtime_id: Uuid::new_v4().to_string(),
            resurfacing_sink: None,
            resurfacing_wake: ResurfacingWakeHandle::default(),
            intent_scorer: Arc::new(NoSemanticIntent),
            enrichment_lock: AsyncMutex::new(()),
            startup_catch_up: None,
        })
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn new_with_resolver(
        workspace: ArtifactV2Workspace,
        resolver: Arc<dyn ObservableContentResolver>,
        settings: ObservableSourceSettings,
    ) -> Result<Self> {
        settings.validate_bounds()?;
        let run_slots = Semaphore::new(settings.max_concurrency);
        let notes_store = NotesSettingsStore::with_workspace_layout(workspace.clone());
        Ok(Self {
            workspace,
            notes_store,
            resolver,
            settings,
            scope_locks: Mutex::new(HashMap::new()),
            run_slots,
            metrics: Mutex::new(ObservableSourceMetricsSnapshot::default()),
            runtime_id: Uuid::new_v4().to_string(),
            resurfacing_sink: None,
            resurfacing_wake: ResurfacingWakeHandle::default(),
            intent_scorer: Arc::new(NoSemanticIntent),
            enrichment_lock: AsyncMutex::new(()),
            startup_catch_up: None,
        })
    }

    pub fn with_resurfacing_sink(
        mut self,
        store: Arc<dyn ResurfacingSink>,
        wake: ResurfacingWakeHandle,
    ) -> Self {
        self.resurfacing_sink = Some(store);
        self.resurfacing_wake = wake;
        self
    }

    /// Attach the semantic side of the hybrid interest filter. Without this the
    /// runtime uses the keyword-only [`NoSemanticIntent`] default.
    pub fn with_intent_scorer(mut self, scorer: Arc<dyn SemanticIntentScorer>) -> Self {
        self.intent_scorer = scorer;
        self
    }

    /// Convenience wiring for the production embedding-backed intent scorer.
    /// Exposes only public types (the shared [`OllamaEmbedder`] + a query
    /// budget) so the bin can attach it without naming the crate-internal
    /// [`SemanticIntentScorer`] trait. Mirrors the resurfacing centrality embed
    /// budget at the call site.
    pub fn with_embedding_intent_scorer(
        self,
        embedder: OllamaEmbedder,
        query_timeout: Duration,
    ) -> Self {
        self.with_intent_scorer(Arc::new(EmbeddingIntentScorer::new(
            embedder,
            query_timeout,
        )))
    }

    pub fn with_startup_catch_up(mut self, controller: Arc<ObserveCatchUpController>) -> Self {
        self.startup_catch_up = Some(controller);
        self
    }

    pub fn metrics(&self) -> ObservableSourceMetricsSnapshot {
        self.metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub async fn list_observability(
        &self,
        principal: &str,
        workspace: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ObservationSourceObservabilityPage, SubscriptionMutationError> {
        validate_page(limit)?;
        let scope_lock = self.scope_lock(principal, workspace);
        let _guard = scope_lock.lock().await;
        let subscriptions = self
            .load_file_unlocked(principal, workspace)
            .await?
            .subscriptions;
        let observability = self
            .load_observability_file_unlocked(principal, workspace)
            .await?;
        let mut items = subscriptions
            .into_iter()
            .map(|subscription| {
                let aggregate = observability
                    .sources
                    .get(&subscription.subscription_id)
                    .cloned()
                    .unwrap_or_default();
                ObservationSourceObservabilitySummary::from_subscription(subscription, aggregate)
            })
            .collect::<Vec<_>>();
        items.sort_by(|a, b| {
            b.enabled
                .cmp(&a.enabled)
                .then_with(|| {
                    a.display_name
                        .to_ascii_lowercase()
                        .cmp(&b.display_name.to_ascii_lowercase())
                })
                .then_with(|| a.subscription_id.cmp(&b.subscription_id))
        });
        let totals = observation_totals(&items);
        let total = items.len();
        let (items, next_cursor) =
            paginate_by_key(items, cursor, limit, |item| &item.subscription_id)?;
        let handoff_backlog = self
            .workspace
            .read_dir_path_or_empty(self.ingress_dir(principal, workspace))
            .await
            .context("reading observable source enrichment backlog")?
            .into_iter()
            .filter(|entry| entry.is_file && entry.file_name.ends_with(".json"))
            .count();
        let failed_handoffs = self
            .workspace
            .read_dir_path_or_empty(self.failed_ingress_dir(principal, workspace))
            .await
            .context("reading failed observable source enrichment handoffs")?
            .into_iter()
            .filter(|entry| entry.is_file && entry.file_name.ends_with(".json"))
            .count();
        Ok(ObservationSourceObservabilityPage {
            items,
            total,
            next_cursor,
            totals,
            handoff_backlog,
            failed_handoffs,
            run_history_retained: observability.recent_runs.len(),
            runtime_metrics: self.metrics(),
        })
    }

    pub async fn list_run_history(
        &self,
        principal: &str,
        workspace: &str,
        subscription_id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ObservationRunRecordPage, SubscriptionMutationError> {
        validate_page(limit)?;
        let scope_lock = self.scope_lock(principal, workspace);
        let _guard = scope_lock.lock().await;
        let subscriptions = self.load_file_unlocked(principal, workspace).await?;
        if !subscriptions
            .subscriptions
            .iter()
            .any(|subscription| subscription.subscription_id == subscription_id)
        {
            return Err(SubscriptionMutationError::NotFound);
        }
        let observability = self
            .load_observability_file_unlocked(principal, workspace)
            .await?;
        let items = observability
            .recent_runs
            .into_iter()
            .filter(|run| run.subscription_id == subscription_id)
            .collect::<Vec<_>>();
        let total = items.len();
        let (items, next_cursor) = paginate_by_key(items, cursor, limit, |item| &item.run_id)?;
        Ok(ObservationRunRecordPage {
            items,
            total,
            next_cursor,
        })
    }

    pub fn start_scheduler(self: &Arc<Self>) -> Option<tokio::task::JoinHandle<()>> {
        if !self.settings.enabled {
            return None;
        }
        let runtime = Arc::clone(self);
        Some(tokio::spawn(async move {
            if !crate::magician_v2::runtime::startup::wait_for_http().await {
                return;
            }
            let mut interval = tokio::time::interval(Duration::from_secs(
                runtime.settings.scheduler_interval_secs,
            ));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                runtime.drain_enrichment_ingress_once().await;
                runtime.run_due_once().await;
                runtime.drain_enrichment_ingress_once().await;
            }
        }))
    }

    pub async fn list_offers(
        &self,
        principal: &str,
        workspace: &str,
        required_action: Option<&str>,
        readiness: Option<ObservableSourceReadiness>,
        subscribed_filter: Option<bool>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ObservableSourceOfferPage, SubscriptionMutationError> {
        validate_page(limit)?;
        let (catalog, mut offers) = self
            .project_offers(principal, workspace, required_action)
            .await?;
        if let Some(readiness) = readiness {
            offers.retain(|offer| offer.readiness == readiness);
        }
        if let Some(subscribed) = subscribed_filter {
            offers.retain(|offer| offer.subscribed == subscribed);
        }
        self.with_metrics(|metrics| {
            metrics.offer_projections = metrics.offer_projections.saturating_add(1);
            for offer in &offers {
                match offer.readiness {
                    ObservableSourceReadiness::Eligible => {
                        metrics.offers_eligible = metrics.offers_eligible.saturating_add(1)
                    },
                    ObservableSourceReadiness::NeedsSetup => {
                        metrics.offers_needs_setup = metrics.offers_needs_setup.saturating_add(1)
                    },
                    ObservableSourceReadiness::Unavailable => {
                        metrics.offers_unavailable = metrics.offers_unavailable.saturating_add(1)
                    },
                }
            }
        });
        let total = offers.len();
        let (items, next_cursor) = paginate_by_key(offers, cursor, limit, |offer| &offer.offer_id)?;
        Ok(ObservableSourceOfferPage {
            items,
            total,
            next_cursor,
            catalog_revision: catalog.revision,
            manifest_issues: catalog.issues,
        })
    }

    pub async fn list_subscriptions(
        &self,
        principal: &str,
        workspace: &str,
        state: Option<ObservationSubscriptionState>,
        enabled: Option<bool>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ObservationSubscriptionPage, SubscriptionMutationError> {
        validate_page(limit)?;
        let now = Utc::now().timestamp_millis();
        let mut items = self.load_file(principal, workspace).await?.subscriptions;
        if let Some(state) = state {
            items.retain(|item| item.state(now) == state);
        }
        if let Some(enabled) = enabled {
            items.retain(|item| item.enabled == enabled);
        }
        items.sort_by(|a, b| {
            b.enabled
                .cmp(&a.enabled)
                .then_with(|| {
                    a.display_name
                        .to_ascii_lowercase()
                        .cmp(&b.display_name.to_ascii_lowercase())
                })
                .then_with(|| a.subscription_id.cmp(&b.subscription_id))
        });
        let total = items.len();
        let (items, next_cursor) =
            paginate_by_key(items, cursor, limit, |item| &item.subscription_id)?;
        Ok(ObservationSubscriptionPage {
            items,
            total,
            next_cursor,
        })
    }

    pub async fn put_subscription(
        &self,
        principal: &str,
        workspace: &str,
        requested_id: &str,
        input: PutObservationSubscription,
    ) -> Result<ObservationSubscription, SubscriptionMutationError> {
        if !self.settings.enabled {
            return Err(SubscriptionMutationError::Unavailable);
        }
        if input.custom_rss.is_some() && (input.source_id.is_some() || input.profile_id.is_some()) {
            return Err(SubscriptionMutationError::Invalid(
                "custom RSS cannot also name a manifest source/profile".into(),
            ));
        }
        let intent = normalize_intent(input.intent)?;
        let resolved = if let Some(custom) = input.custom_rss.as_ref() {
            self.resolve_custom_offer(principal, workspace, custom)
                .await?
        } else {
            let source_id = input.source_id.as_deref().ok_or_else(|| {
                SubscriptionMutationError::Invalid("source_id is required".into())
            })?;
            let profile_id = input.profile_id.as_deref().ok_or_else(|| {
                SubscriptionMutationError::Invalid("profile_id is required".into())
            })?;
            self.resolve_manifest_offer(principal, workspace, source_id, profile_id)
                .await?
        };
        if input
            .source_revision
            .as_deref()
            .is_some_and(|revision| revision != resolved.source_revision)
        {
            return Err(SubscriptionMutationError::StaleSource);
        }
        if !resolved.supported_cadence.contains(&input.cadence) {
            return Err(SubscriptionMutationError::Invalid(
                "cadence is not supported by this source profile".into(),
            ));
        }
        let max_candidates = input
            .max_candidates_per_run
            .unwrap_or(resolved.limits.max_candidates_per_run);
        let max_selected = input
            .max_selected_per_run
            .unwrap_or(resolved.limits.max_selected_per_run);
        if max_candidates == 0
            || max_candidates > resolved.limits.max_candidates_per_run
            || max_selected == 0
            || max_selected > resolved.limits.max_selected_per_run
            || max_selected > max_candidates
        {
            return Err(SubscriptionMutationError::Invalid(
                "requested per-run limits exceed the source profile".into(),
            ));
        }
        let deterministic_id = subscription_id(
            principal,
            workspace,
            &resolved.source_id,
            &resolved.profile_id,
            resolved.targets.first().map(String::as_str),
        );
        if requested_id != "auto" && requested_id != deterministic_id {
            return Err(SubscriptionMutationError::Invalid(
                "subscription id does not match the source/profile identity".into(),
            ));
        }

        let scope_lock = self.scope_lock(principal, workspace);
        let _guard = scope_lock.lock().await;
        let mut file = self.load_file_unlocked(principal, workspace).await?;
        let now = Utc::now().timestamp_millis();
        if let Some(existing) = file
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.subscription_id == deterministic_id)
        {
            if requested_id != "auto" && input.expected_revision.is_none() {
                return Err(SubscriptionMutationError::Invalid(
                    "expected_revision is required when updating a subscription by id".into(),
                ));
            }
            if input
                .expected_revision
                .is_some_and(|revision| revision != existing.revision)
            {
                return Err(SubscriptionMutationError::RevisionConflict);
            }
            let acquisition_changed = existing.source_revision != resolved.source_revision
                || existing.profile_revision != resolved.profile_revision
                || existing.action_bindings != resolved.action_bindings
                || existing.targets != resolved.targets;
            let changed = existing.enabled != input.enabled
                || existing.cadence != input.cadence
                || existing.supported_cadence != resolved.supported_cadence
                || existing.intent != intent
                || existing.max_candidates_per_run != max_candidates
                || existing.max_selected_per_run != max_selected
                || acquisition_changed;
            if changed {
                existing.enabled = input.enabled;
                existing.cadence = input.cadence;
                existing.supported_cadence = resolved.supported_cadence;
                existing.intent = intent;
                existing.max_candidates_per_run = max_candidates;
                existing.max_selected_per_run = max_selected;
                existing.source_revision = resolved.source_revision;
                existing.profile_revision = resolved.profile_revision;
                existing.action_bindings = resolved.action_bindings;
                existing.targets = resolved.targets;
                existing.display_name = resolved.display_name;
                existing.category = resolved.category;
                existing.updated_at_ms = now;
                existing.next_run_at_ms = if input.enabled {
                    now
                } else {
                    now.saturating_add(input.cadence.interval_ms())
                };
                existing.revision = existing.revision.saturating_add(1);
                existing.lease_owner = None;
                existing.lease_expires_at_ms = None;
                if acquisition_changed {
                    existing.cursor = None;
                    existing.validators.clear();
                    existing.last_observed_identity = None;
                    existing.seen_fingerprints.clear();
                }
            }
            let output = existing.clone();
            self.save_file_unlocked(principal, workspace, &file).await?;
            return Ok(output);
        }
        if input.expected_revision.is_some() {
            return Err(SubscriptionMutationError::RevisionConflict);
        }
        if file.subscriptions.len() >= MAX_SUBSCRIPTIONS_PER_SCOPE {
            return Err(SubscriptionMutationError::Invalid(
                "scope has reached the observable source subscription limit".into(),
            ));
        }
        let subscription = ObservationSubscription {
            schema_version: SUBSCRIPTION_SCHEMA_VERSION,
            subscription_id: deterministic_id,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            source_id: resolved.source_id,
            source_revision: resolved.source_revision,
            profile_id: resolved.profile_id,
            profile_revision: resolved.profile_revision,
            display_name: resolved.display_name,
            category: resolved.category,
            enabled: input.enabled,
            custom: input.custom_rss.is_some(),
            cadence: input.cadence,
            supported_cadence: resolved.supported_cadence,
            next_run_at_ms: now,
            intent,
            max_candidates_per_run: max_candidates,
            max_selected_per_run: max_selected,
            action_bindings: resolved.action_bindings,
            targets: resolved.targets,
            cursor: None,
            validators: BTreeMap::new(),
            last_observed_identity: None,
            seen_fingerprints: Vec::new(),
            last_run_started_at_ms: None,
            last_success_at_ms: None,
            consecutive_failures: 0,
            last_error_class: None,
            lease_owner: None,
            lease_expires_at_ms: None,
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
        };
        file.subscriptions.push(subscription.clone());
        self.save_file_unlocked(principal, workspace, &file).await?;
        Ok(subscription)
    }

    pub async fn delete_subscription(
        &self,
        principal: &str,
        workspace: &str,
        subscription_id: &str,
    ) -> Result<bool, SubscriptionMutationError> {
        let scope_lock = self.scope_lock(principal, workspace);
        let _guard = scope_lock.lock().await;
        let mut file = self.load_file_unlocked(principal, workspace).await?;
        let before = file.subscriptions.len();
        file.subscriptions
            .retain(|subscription| subscription.subscription_id != subscription_id);
        let removed = file.subscriptions.len() != before;
        if removed {
            self.save_file_unlocked(principal, workspace, &file).await?;
            match self
                .load_observability_file_unlocked(principal, workspace)
                .await
            {
                Ok(mut observability) => {
                    observability.sources.remove(subscription_id);
                    observability
                        .recent_runs
                        .retain(|run| run.subscription_id != subscription_id);
                    if let Err(error) = self
                        .save_observability_file_unlocked(principal, workspace, &observability)
                        .await
                    {
                        tracing::warn!(
                            principal,
                            workspace,
                            subscription_id,
                            error = %error,
                            "deleted observable source left observability history behind"
                        );
                    }
                },
                Err(error) => tracing::warn!(
                    principal,
                    workspace,
                    subscription_id,
                    error = %error,
                    "deleted observable source observability history could not be read"
                ),
            }
        }
        Ok(removed)
    }

    pub async fn run_now(
        self: &Arc<Self>,
        principal: &str,
        workspace: &str,
        subscription_id: &str,
    ) -> Result<ObservationRunOutcome, SubscriptionMutationError> {
        if !self.settings.enabled {
            return Err(SubscriptionMutationError::Unavailable);
        }
        self.run_subscription(principal, workspace, subscription_id, true)
            .await
    }

    async fn run_due_once(self: &Arc<Self>) {
        let scopes = self.workspace.list_tenant_scopes();
        let now = Utc::now().timestamp_millis();
        let mut due = Vec::new();
        for (principal, workspace) in scopes {
            match self.load_file(&principal, &workspace).await {
                Ok(file) => {
                    due.extend(
                        file.subscriptions
                            .into_iter()
                            .filter(|subscription| {
                                subscription.enabled && subscription.next_run_at_ms <= now
                            })
                            .map(|subscription| {
                                (
                                    principal.clone(),
                                    workspace.clone(),
                                    subscription.subscription_id,
                                )
                            }),
                    );
                },
                Err(error) => tracing::warn!(
                    principal,
                    workspace,
                    error = ?error,
                    "observable source scheduler could not read subscriptions"
                ),
            }
        }
        stream::iter(due)
            .for_each_concurrent(
                self.settings.max_concurrency,
                |(principal, workspace, id)| {
                    let runtime = Arc::clone(self);
                    async move {
                        if let Err(error) = runtime
                            .run_scheduled_subscription(&principal, &workspace, &id)
                            .await
                        {
                            tracing::warn!(
                                principal,
                                workspace,
                                subscription_id = id,
                                error_class = error_class(&error),
                                "observable source scheduled run failed"
                            );
                        }
                    }
                },
            )
            .await;
    }

    async fn run_subscription(
        self: &Arc<Self>,
        principal: &str,
        workspace: &str,
        subscription_id: &str,
        force: bool,
    ) -> Result<ObservationRunOutcome, SubscriptionMutationError> {
        self.run_subscription_with_policy(principal, workspace, subscription_id, force, None)
            .await
    }

    async fn run_scheduled_subscription(
        self: &Arc<Self>,
        principal: &str,
        workspace: &str,
        subscription_id: &str,
    ) -> Result<ObservationRunOutcome, SubscriptionMutationError> {
        let Some(controller) = self.startup_catch_up.as_ref() else {
            return self
                .run_subscription_with_policy(principal, workspace, subscription_id, false, None)
                .await;
        };
        let subscription = self
            .load_file(principal, workspace)
            .await?
            .subscriptions
            .into_iter()
            .find(|item| item.subscription_id == subscription_id)
            .ok_or(SubscriptionMutationError::NotFound)?;
        let notes = is_notes_binding(subscription.action_bindings.first());
        let source_id = format!("observe:{}", subscription.subscription_id);
        let replay_mode = if notes {
            CatchUpReplayMode::CheckpointedReplay
        } else {
            CatchUpReplayMode::CurrentSnapshotOnly
        };
        let limitation = if notes {
            "Only notes published into the configured observation source are eligible."
        } else {
            "The feed exposes its current snapshot; entries no longer present cannot be replayed."
        };
        let decision = controller
            .begin(
                principal,
                workspace,
                &source_id,
                &subscription.display_name,
                if notes { "notes" } else { "candidates" },
                replay_mode,
                subscription.max_candidates_per_run,
                limitation,
            )
            .await;
        let mut skipped_catch_up = false;
        let (policy, admission) = match decision {
            CatchUpDecision::Admit(admission) => (
                Some(AutomaticObservationPolicy {
                    max_candidates: admission.max_items,
                    retain_from_ms: admission.historical_floor_ms,
                    // Restart a catch-up from the newest page exactly once so
                    // new entries cannot sit behind a stale pre-boot offset.
                    // Checkpointed notes retain later page continuations.
                    ignore_cursor: admission.initial_batch,
                    discard_next_cursor: !notes,
                }),
                Some(admission),
            ),
            CatchUpDecision::SkipHistorical {
                retain_from_ms,
                reason,
            } => {
                tracing::debug!(
                    principal,
                    workspace,
                    subscription_id,
                    reason,
                    "historical observable-source catch-up skipped; checking only post-boot \
                     entries"
                );
                skipped_catch_up = true;
                (
                    Some(AutomaticObservationPolicy {
                        max_candidates: subscription.max_candidates_per_run,
                        retain_from_ms,
                        ignore_cursor: true,
                        discard_next_cursor: true,
                    }),
                    None,
                )
            },
            CatchUpDecision::Normal => (
                Some(AutomaticObservationPolicy {
                    max_candidates: subscription.max_candidates_per_run,
                    retain_from_ms: controller.boot_started_at_ms(),
                    ignore_cursor: false,
                    discard_next_cursor: true,
                }),
                None,
            ),
        };
        // The controller's deadline stops admitting additional startup work.
        // Once this runtime has durably acquired a source lease, let its
        // existing provider timeout and normal failure path finish the lease;
        // cancelling this future here would strand the lease until expiry.
        let result = self
            .run_subscription_with_policy(principal, workspace, subscription_id, false, policy)
            .await;
        if let Some(admission) = admission {
            match &result {
                Ok(outcome) if notes => controller.complete_with_exhaustion(
                    principal,
                    workspace,
                    admission.clone(),
                    outcome.discovered,
                    None,
                    outcome.next_cursor.is_none() || outcome.discovered < admission.max_items,
                ),
                Ok(outcome) => {
                    controller.complete(principal, workspace, admission, outcome.discovered, None)
                },
                Err(error) => controller.complete(
                    principal,
                    workspace,
                    admission,
                    0,
                    Some(error_class(error)),
                ),
            }
        } else if skipped_catch_up {
            controller.finish_skipped(
                principal,
                workspace,
                &source_id,
                &subscription.display_name,
                if notes { "notes" } else { "candidates" },
                replay_mode,
                limitation,
                result.as_ref().err().map(error_class),
                true,
            );
        }
        result
    }

    /// The single funnel for every observable-source run — manual `run_now`,
    /// scheduled sweep, and startup catch-up all land here — so one span here
    /// covers the family without a span per entry point.
    ///
    /// `ambient`, and the same class on all three entry points. This is
    /// deterministic polling of sources the operator subscribed to: it observes
    /// the outside world and files candidates for later, which is the family
    /// `workload_for_operation` already puts `ScreenObservation` and friends
    /// in, not the runtime-maintenance family
    /// `attention_rank_recompute_pass` and `taste_capture_sweep` sit in.
    /// `run_now` does not change what the work is, exactly as the manual
    /// `POST /ambient/distill` route does not change
    /// `ambient_distill_pass`.
    ///
    /// Declared rather than left to inherit because this is a root: the
    /// scheduler drives it from its own task, so there is nothing above it to
    /// inherit from and it was landing in the view's Undeclared lane.
    #[instrument(
        name = "observable_source_run",
        skip_all,
        fields(
            activity_kind = KIND_BACKGROUND,
            workload_class = WORKLOAD_AMBIENT,
            principal = %principal,
            workspace = %workspace,
            subscription_id = %subscription_id,
        )
    )]
    async fn run_subscription_with_policy(
        self: &Arc<Self>,
        principal: &str,
        workspace: &str,
        subscription_id: &str,
        force: bool,
        automatic_policy: Option<AutomaticObservationPolicy>,
    ) -> Result<ObservationRunOutcome, SubscriptionMutationError> {
        let _run_slot = self.run_slots.try_acquire().map_err(|_| {
            self.with_metrics(|metrics| {
                metrics.runs_throttled = metrics.runs_throttled.saturating_add(1)
            });
            SubscriptionMutationError::Busy
        })?;
        let started = Instant::now();
        let leased = self
            .acquire_lease(principal, workspace, subscription_id, force)
            .await?;
        self.with_metrics(|metrics| metrics.runs_started = metrics.runs_started.saturating_add(1));

        let result = match self.execute_leased(&leased, automatic_policy).await {
            Ok(executed) => self.commit_success(&leased, executed).await,
            Err(error) => Err(error),
        };
        let elapsed = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        match result {
            Ok(mut outcome) => {
                outcome.duration_ms = elapsed;
                self.with_metrics(|metrics| {
                    metrics.runs_succeeded = metrics.runs_succeeded.saturating_add(1);
                    metrics.total_latency_ms = metrics.total_latency_ms.saturating_add(elapsed);
                    metrics.candidates_discovered = metrics
                        .candidates_discovered
                        .saturating_add(outcome.discovered as u64);
                    metrics.candidates_deduped = metrics
                        .candidates_deduped
                        .saturating_add(outcome.deduped as u64);
                    metrics.candidates_selected = metrics
                        .candidates_selected
                        .saturating_add(outcome.selected as u64);
                    metrics.enrichment_handoffs = metrics
                        .enrichment_handoffs
                        .saturating_add(outcome.handed_off as u64);
                    if outcome.cursor_advanced {
                        metrics.cursor_advances = metrics.cursor_advances.saturating_add(1);
                    }
                    metrics.modified_targets = metrics
                        .modified_targets
                        .saturating_add(outcome.transport.modified_targets);
                    metrics.not_modified_targets = metrics
                        .not_modified_targets
                        .saturating_add(outcome.transport.not_modified_targets);
                    metrics.response_bytes = metrics
                        .response_bytes
                        .saturating_add(outcome.transport.response_bytes);
                    if let Some(cost) = outcome.cost.as_ref() {
                        let total = metrics
                            .cost_microunits
                            .entry(cost.commodity.clone())
                            .or_default();
                        *total = total.saturating_add(cost.amount_microunits);
                    }
                });
                self.record_run_best_effort(
                    &leased.principal,
                    &leased.workspace,
                    observation_run_record(
                        &leased,
                        if force {
                            ObservationRunTrigger::Manual
                        } else {
                            ObservationRunTrigger::Scheduled
                        },
                        ObservationRunStatus::Succeeded,
                        elapsed,
                        Some(&outcome),
                        None,
                    ),
                )
                .await;
                if outcome.handed_off > 0 {
                    if let Err(error) = self
                        .drain_scope_enrichment(&leased.principal, &leased.workspace)
                        .await
                    {
                        tracing::warn!(
                            principal,
                            workspace,
                            subscription_id,
                            error = %error,
                            "observable source run completed but enrichment drain failed"
                        );
                    }
                }
                tracing::info!(
                    principal,
                    workspace,
                    subscription_id,
                    source_id = leased.source_id,
                    profile_id = leased.profile_id,
                    action_id = outcome.action_id,
                    discovered = outcome.discovered,
                    deduped = outcome.deduped,
                    selected = outcome.selected,
                    handed_off = outcome.handed_off,
                    cursor_advanced = outcome.cursor_advanced,
                    duration_ms = elapsed,
                    "observable source run completed"
                );
                Ok(outcome)
            },
            Err(error) => {
                self.finish_failure(&leased, error_class(&error)).await?;
                self.with_metrics(|metrics| {
                    metrics.runs_failed = metrics.runs_failed.saturating_add(1);
                    metrics.total_latency_ms = metrics.total_latency_ms.saturating_add(elapsed);
                    if matches!(
                        &error,
                        SubscriptionMutationError::Unavailable
                            | SubscriptionMutationError::StaleSource
                    ) {
                        metrics.policy_denials = metrics.policy_denials.saturating_add(1);
                    }
                });
                self.record_run_best_effort(
                    &leased.principal,
                    &leased.workspace,
                    observation_run_record(
                        &leased,
                        if force {
                            ObservationRunTrigger::Manual
                        } else {
                            ObservationRunTrigger::Scheduled
                        },
                        if matches!(&error, SubscriptionMutationError::Busy) {
                            ObservationRunStatus::Cancelled
                        } else {
                            ObservationRunStatus::Failed
                        },
                        elapsed,
                        None,
                        Some(error_class(&error)),
                    ),
                )
                .await;
                tracing::warn!(
                    principal,
                    workspace,
                    subscription_id,
                    source_id = leased.source_id,
                    profile_id = leased.profile_id,
                    error_class = error_class(&error),
                    duration_ms = elapsed,
                    "observable source run failed"
                );
                Err(error)
            },
        }
    }

    async fn execute_leased(
        &self,
        subscription: &ObservationSubscription,
        automatic_policy: Option<AutomaticObservationPolicy>,
    ) -> Result<ExecutedObservation, SubscriptionMutationError> {
        let resolved = if subscription.custom {
            self.resolve_custom_offer(
                &subscription.principal,
                &subscription.workspace,
                &CustomRssSubscription {
                    display_name: subscription.display_name.clone(),
                    feed_url: subscription
                        .targets
                        .first()
                        .cloned()
                        .ok_or_else(|| SubscriptionMutationError::Unavailable)?,
                },
            )
            .await?
        } else {
            self.resolve_manifest_offer(
                &subscription.principal,
                &subscription.workspace,
                &subscription.source_id,
                &subscription.profile_id,
            )
            .await?
        };
        if resolved.action_bindings != subscription.action_bindings
            || resolved.targets != subscription.targets
            || resolved.source_revision != subscription.source_revision
            || resolved.profile_revision != subscription.profile_revision
        {
            return Err(SubscriptionMutationError::StaleSource);
        }
        if resolved.action_bindings.len() != 1 {
            return Err(SubscriptionMutationError::Invalid(
                "Observe polling currently requires one exact discovery action".into(),
            ));
        }
        let binding = &resolved.action_bindings[0];
        let page = if is_notes_binding(Some(binding)) {
            self.discover_notes_page(
                subscription,
                automatic_policy
                    .map(|policy| policy.max_candidates)
                    .unwrap_or(subscription.max_candidates_per_run),
                automatic_policy.is_some_and(|policy| policy.ignore_cursor),
            )
            .await?
        } else {
            let service = self
                .resolver
                .resolve(
                    subscription.principal.clone(),
                    subscription.workspace.clone(),
                )
                .await
                .context("resolving observable source acquisition service")?;
            let descriptor = service
                .catalog()
                .discovery
                .into_iter()
                .find(|descriptor| descriptor.adapter_id == binding.adapter_id)
                .ok_or(SubscriptionMutationError::Unavailable)?;
            service
                .discover(
                    &binding.adapter_id,
                    &DiscoveryRequest {
                        principal: subscription.principal.clone(),
                        workspace: subscription.workspace.clone(),
                        intent: subscription.intent.clone(),
                        query: None,
                        targets: subscription.targets.clone(),
                        cursor: if descriptor.capabilities.cursor
                            && !automatic_policy.is_some_and(|policy| policy.ignore_cursor)
                        {
                            subscription.cursor.clone()
                        } else {
                            None
                        },
                        validators: if descriptor.capabilities.conditional_fetch {
                            subscription.validators.clone()
                        } else {
                            BTreeMap::new()
                        },
                        limit: automatic_policy
                            .map(|policy| policy.max_candidates)
                            .unwrap_or(subscription.max_candidates_per_run),
                        freshness: FreshnessPolicy::Fresh,
                        remote_query_policy: RemoteDataPolicy::Allow,
                        invocation_source: ContentInvocationSource::ObservedSource,
                        options: BTreeMap::new(),
                    },
                )
                .await
                .context("running pinned observable source action")?
        };
        let mut page = page;
        if let Some(policy) = automatic_policy {
            page.items.retain(|candidate| {
                candidate
                    .published_at_ms
                    .unwrap_or(candidate.observed_at_ms)
                    >= policy.retain_from_ms
            });
            page.items.truncate(policy.max_candidates);
            if policy.discard_next_cursor {
                // Keep automatic polling on the newest post-boot page for the
                // rest of this boot. Persisting a deeper catalog offset here
                // would let the next tick walk the history we just skipped.
                page.next_cursor = None;
            }
        }
        let discovered = page.items.len();
        let seen = subscription
            .seen_fingerprints
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut unique = BTreeMap::new();
        for candidate in page.items {
            let fingerprint = candidate_fingerprint(&candidate);
            if !seen.contains(fingerprint.as_str()) {
                unique.entry(fingerprint).or_insert(candidate);
            }
        }
        let deduped = discovered.saturating_sub(unique.len());
        let selected = select_candidates(
            unique,
            subscription.intent.as_deref(),
            subscription.max_selected_per_run,
            self.intent_scorer.as_ref(),
        )
        .await;

        let mut handoffs = Vec::with_capacity(selected.len());
        for selected_candidate in &selected {
            let receipt = ObserveMatchReceipt {
                schema_version: 1,
                receipt_id: blake3::hash(
                    format!(
                        "{}:{}:{}",
                        subscription.subscription_id,
                        selected_candidate.fingerprint,
                        subscription.revision
                    )
                    .as_bytes(),
                )
                .to_hex()
                .to_string(),
                subscription_id: subscription.subscription_id.clone(),
                source_id: subscription.source_id.clone(),
                profile_id: subscription.profile_id.clone(),
                action_id: binding.action_id.clone(),
                candidate_fingerprint: selected_candidate.fingerprint.clone(),
                selected_at_ms: Utc::now().timestamp_millis(),
                relevance_score: selected_candidate.relevance_score,
            };
            let envelope = ObservedContentEnvelope {
                schema_version: 1,
                principal: subscription.principal.clone(),
                workspace: subscription.workspace.clone(),
                source_id: subscription.source_id.clone(),
                source_revision: subscription.source_revision.clone(),
                profile_id: subscription.profile_id.clone(),
                profile_revision: subscription.profile_revision.clone(),
                subscription_id: subscription.subscription_id.clone(),
                action_id: binding.action_id.clone(),
                candidate: selected_candidate.candidate.clone(),
                selection: receipt,
                admitted_at_ms: Utc::now().timestamp_millis(),
            };
            handoffs.push(envelope);
        }
        Ok(ExecutedObservation {
            outcome: ObservationRunOutcome {
                subscription_id: subscription.subscription_id.clone(),
                action_id: binding.action_id.clone(),
                discovered,
                deduped,
                selected: selected.len(),
                handed_off: 0,
                cursor_advanced: page.next_cursor != subscription.cursor,
                duration_ms: 0,
                cost: page.cost,
                transport: page.transport,
                next_cursor: page.next_cursor,
            },
            handoffs,
            next_validators: page.validators,
        })
    }

    async fn discover_notes_page(
        &self,
        subscription: &ObservationSubscription,
        limit: usize,
        ignore_cursor: bool,
    ) -> Result<DiscoveryPage, SubscriptionMutationError> {
        let offset = if ignore_cursor {
            0
        } else {
            subscription
                .cursor
                .as_deref()
                .map(|cursor| {
                    cursor
                        .strip_prefix("notes:")
                        .and_then(|value| value.parse::<usize>().ok())
                        .filter(|value| *value <= NOTES_MAX_CURSOR_OFFSET)
                        .ok_or_else(|| {
                            SubscriptionMutationError::Invalid(
                                "Notes observation checkpoint is invalid".into(),
                            )
                        })
                })
                .transpose()?
                .unwrap_or_default()
        };
        let batch = self
            .notes_store
            .discover_observation_notes(
                &subscription.principal,
                &subscription.workspace,
                offset,
                limit,
            )
            .await
            .context("discovering scoped Markdown notes")?;
        let observed_at_ms = Utc::now().timestamp_millis();
        let mut items = Vec::with_capacity(batch.items.len());
        for note in batch.items {
            let mut metadata = BTreeMap::new();
            metadata.insert("observation_source_kind".into(), serde_json::json!("note"));
            metadata.insert("note_provider".into(), serde_json::json!(note.provider));
            metadata.insert("note_path".into(), serde_json::json!(note.relative_path));
            if let Some(open_url) = note.open_url {
                metadata.insert("open_url".into(), serde_json::json!(open_url));
            }
            let candidate = ContentCandidate {
                schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                identity: SourceIdentity::new(NOTES_ADAPTER_ID, &note.source_ref)
                    .context("building Notes observation identity")?,
                title: note.title,
                cheap_text: note.markdown,
                canonical_url: None,
                published_at_ms: Some(note.modified_at_ms),
                observed_at_ms,
                privacy: ContentPrivacy::Private,
                content_hash: Some(note.content_hash),
                provenance: ContentProvenance {
                    source_label: "Notes".into(),
                    source_url: None,
                    retrieved_by: NOTES_ADAPTER_ID.into(),
                },
                metadata,
            };
            candidate
                .validate()
                .context("validating Notes observation candidate")?;
            items.push(candidate);
        }
        let modified = !items.is_empty();
        Ok(DiscoveryPage {
            items,
            next_cursor: batch.next_offset.map(|offset| format!("notes:{offset}")),
            validators: BTreeMap::new(),
            cost: None,
            transport: DiscoveryTransportStats {
                modified_targets: u64::from(modified),
                not_modified_targets: u64::from(!modified),
                response_bytes: batch.response_bytes,
            },
        })
    }

    async fn acquire_lease(
        &self,
        principal: &str,
        workspace: &str,
        subscription_id: &str,
        force: bool,
    ) -> Result<ObservationSubscription, SubscriptionMutationError> {
        let scope_lock = self.scope_lock(principal, workspace);
        let _guard = scope_lock.lock().await;
        let mut file = self.load_file_unlocked(principal, workspace).await?;
        let now = Utc::now().timestamp_millis();
        let subscription = file
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.subscription_id == subscription_id)
            .ok_or(SubscriptionMutationError::NotFound)?;
        if !subscription.enabled || (!force && subscription.next_run_at_ms > now) {
            return Err(SubscriptionMutationError::Busy);
        }
        if subscription
            .lease_expires_at_ms
            .is_some_and(|expires| expires > now)
        {
            self.with_metrics(|metrics| {
                metrics.leases_skipped = metrics.leases_skipped.saturating_add(1)
            });
            return Err(SubscriptionMutationError::Busy);
        }
        if force
            && subscription
                .last_run_started_at_ms
                .is_some_and(|started| now.saturating_sub(started) < RUN_NOW_COOLDOWN_MS)
        {
            return Err(SubscriptionMutationError::Busy);
        }
        subscription.lease_owner = Some(format!("{}:{}", self.runtime_id, Uuid::new_v4()));
        subscription.lease_expires_at_ms =
            Some(now.saturating_add(self.settings.lease_ttl_secs.saturating_mul(1_000) as i64));
        subscription.last_run_started_at_ms = Some(now);
        subscription.updated_at_ms = now;
        subscription.revision = subscription.revision.saturating_add(1);
        let leased = subscription.clone();
        self.save_file_unlocked(principal, workspace, &file).await?;
        Ok(leased)
    }

    async fn commit_success(
        &self,
        leased: &ObservationSubscription,
        mut executed: ExecutedObservation,
    ) -> Result<ObservationRunOutcome, SubscriptionMutationError> {
        let scope_lock = self.scope_lock(&leased.principal, &leased.workspace);
        let _guard = scope_lock.lock().await;
        let mut file = self
            .load_file_unlocked(&leased.principal, &leased.workspace)
            .await?;
        let current_index = file
            .subscriptions
            .iter()
            .position(|subscription| subscription.subscription_id == leased.subscription_id)
            .ok_or(SubscriptionMutationError::NotFound)?;
        let current = &file.subscriptions[current_index];
        if current.lease_owner != leased.lease_owner
            || !current.enabled
            || current.revision != leased.revision
            || current
                .lease_expires_at_ms
                .is_none_or(|expires| expires <= Utc::now().timestamp_millis())
        {
            return Err(SubscriptionMutationError::Busy);
        }

        let mut accepted_fingerprints = Vec::with_capacity(executed.handoffs.len());
        for envelope in &executed.handoffs {
            if self.persist_enrichment_handoff(envelope).await? {
                executed.outcome.handed_off = executed.outcome.handed_off.saturating_add(1);
            }
            accepted_fingerprints.push(envelope.selection.candidate_fingerprint.clone());
        }

        let current = &mut file.subscriptions[current_index];
        let now = Utc::now().timestamp_millis();
        current.lease_owner = None;
        current.lease_expires_at_ms = None;
        current.last_success_at_ms = Some(now);
        current.consecutive_failures = 0;
        current.last_error_class = None;
        current.cursor = executed.outcome.next_cursor.clone();
        current.validators = executed.next_validators;
        current.next_run_at_ms = now.saturating_add(current.cadence.interval_ms());
        current.updated_at_ms = now;
        current.revision = current.revision.saturating_add(1);
        let last_accepted = accepted_fingerprints.last().cloned();
        for fingerprint in accepted_fingerprints {
            if !current.seen_fingerprints.contains(&fingerprint) {
                current.seen_fingerprints.push(fingerprint);
            }
        }
        if current.seen_fingerprints.len() > MAX_SEEN_FINGERPRINTS {
            let drop_count = current.seen_fingerprints.len() - MAX_SEEN_FINGERPRINTS;
            current.seen_fingerprints.drain(..drop_count);
        }
        if let Some(last_accepted) = last_accepted {
            current.last_observed_identity = Some(last_accepted);
        }
        self.save_file_unlocked(&leased.principal, &leased.workspace, &file)
            .await?;
        Ok(executed.outcome)
    }

    async fn finish_failure(
        &self,
        leased: &ObservationSubscription,
        class: &str,
    ) -> Result<(), SubscriptionMutationError> {
        let scope_lock = self.scope_lock(&leased.principal, &leased.workspace);
        let _guard = scope_lock.lock().await;
        let mut file = self
            .load_file_unlocked(&leased.principal, &leased.workspace)
            .await?;
        let Some(current) = file
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.subscription_id == leased.subscription_id)
        else {
            return Ok(());
        };
        if current.lease_owner != leased.lease_owner || current.revision != leased.revision {
            return Ok(());
        }
        let now = Utc::now().timestamp_millis();
        current.lease_owner = None;
        current.lease_expires_at_ms = None;
        current.consecutive_failures = current.consecutive_failures.saturating_add(1);
        current.last_error_class = Some(class.to_string());
        let exponent = current.consecutive_failures.saturating_sub(1).min(8);
        let retry_ms = 60_000i64.saturating_mul(1i64 << exponent);
        current.next_run_at_ms = now.saturating_add(retry_ms.min(current.cadence.interval_ms()));
        current.updated_at_ms = now;
        current.revision = current.revision.saturating_add(1);
        self.save_file_unlocked(&leased.principal, &leased.workspace, &file)
            .await
    }

    async fn resolve_manifest_offer(
        &self,
        principal: &str,
        workspace: &str,
        source_id: &str,
        profile_id: &str,
    ) -> Result<ObservableSourceOffer, SubscriptionMutationError> {
        let (_, offers) = self.project_offers(principal, workspace, None).await?;
        offers
            .into_iter()
            .find(|offer| offer.source_id == source_id && offer.profile_id == profile_id)
            .filter(|offer| offer.readiness == ObservableSourceReadiness::Eligible)
            .ok_or(SubscriptionMutationError::Unavailable)
    }

    async fn project_offers(
        &self,
        principal: &str,
        workspace: &str,
        required_action: Option<&str>,
    ) -> Result<(ObservableCatalog, Vec<ObservableSourceOffer>), SubscriptionMutationError> {
        let catalog = self.catalog(principal, workspace);
        let service = self
            .resolver
            .resolve(principal.to_string(), workspace.to_string())
            .await
            .context("resolving observable source action catalog")?;
        let subscriptions = self.load_file(principal, workspace).await?;
        let subscribed = subscriptions
            .subscriptions
            .iter()
            .filter(|subscription| subscription.enabled)
            .map(|subscription| {
                (
                    subscription.source_id.clone(),
                    subscription.profile_id.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        let mut offers = project_observable_source_offers(
            &catalog,
            &service.catalog(),
            &self.settings.source_catalog,
            &subscribed,
            required_action,
        );
        if required_action.is_none_or(|required| required == NOTES_ACTION_ID) {
            offers.push(
                self.notes_offer(
                    principal,
                    workspace,
                    subscribed.contains(&(NOTES_SOURCE_ID.into(), NOTES_PROFILE_ID.into())),
                )
                .await?,
            );
        }
        if !self.settings.enabled {
            for offer in &mut offers {
                offer.readiness = ObservableSourceReadiness::Unavailable;
                offer.unavailable_reason = Some(ObservableUnavailableReason::FeatureDisabled);
                offer.action_bindings.clear();
            }
        }
        Ok((catalog, offers))
    }

    async fn notes_offer(
        &self,
        principal: &str,
        workspace: &str,
        subscribed: bool,
    ) -> Result<ObservableSourceOffer, SubscriptionMutationError> {
        let notes = self
            .notes_store
            .observation_catalog(principal, workspace)
            .await
            .context("resolving scoped Notes observation target")?;
        let profile_revision = blake3::hash(
            format!(
                "{}:{}:{}:{:?}",
                notes.source_revision,
                NOTES_PROFILE_ID,
                NOTES_ACTION_ID,
                [
                    ObservationCadence::Hourly,
                    ObservationCadence::TwiceDaily,
                    ObservationCadence::Daily,
                ]
            )
            .as_bytes(),
        )
        .to_hex()
        .to_string();
        let provider_label = match notes.providers.as_slice() {
            [] => "the scoped Notes space".to_string(),
            [provider] => provider.replace('_', " "),
            providers => providers
                .iter()
                .map(|provider| provider.replace('_', " "))
                .collect::<Vec<_>>()
                .join(" and "),
        };
        Ok(ObservableSourceOffer {
            offer_id: format!("{NOTES_SOURCE_ID}:{NOTES_PROFILE_ID}"),
            source_id: NOTES_SOURCE_ID.into(),
            source_revision: notes.source_revision,
            profile_id: NOTES_PROFILE_ID.into(),
            profile_revision,
            display_name: "Notes".into(),
            category: "knowledge".into(),
            description: format!(
                "Observe new and edited Markdown notes from {provider_label} through the same \
                 private curation pipeline."
            ),
            readiness: if notes.enabled {
                ObservableSourceReadiness::Eligible
            } else {
                ObservableSourceReadiness::NeedsSetup
            },
            unavailable_reason: (!notes.enabled)
                .then_some(ObservableUnavailableReason::MissingConfiguration),
            subscribed,
            supported_cadence: vec![
                ObservationCadence::Hourly,
                ObservationCadence::TwiceDaily,
                ObservationCadence::Daily,
            ],
            default_cadence: ObservationCadence::Hourly,
            limits: ObservationProfileLimits {
                max_candidates_per_run: 100,
                max_selected_per_run: 20,
            },
            targets: vec![NOTES_TARGET.into()],
            action_bindings: notes
                .enabled
                .then(|| {
                    vec![ObservableActionBinding {
                        action_id: NOTES_ACTION_ID.into(),
                        adapter_id: NOTES_ADAPTER_ID.into(),
                    }]
                })
                .unwrap_or_default(),
        })
    }

    async fn resolve_custom_offer(
        &self,
        principal: &str,
        workspace: &str,
        custom: &CustomRssSubscription,
    ) -> Result<ObservableSourceOffer, SubscriptionMutationError> {
        validate_custom_name(&custom.display_name)?;
        validate_public_http_url(&custom.feed_url)
            .map_err(|error| SubscriptionMutationError::Invalid(error.to_string()))?;
        let url = canonicalize_http_url(&custom.feed_url)
            .map_err(|error| SubscriptionMutationError::Invalid(error.to_string()))?;
        let service = self
            .resolver
            .resolve(principal.to_string(), workspace.to_string())
            .await
            .context("resolving custom RSS action")?;
        let descriptor = service
            .catalog()
            .discovery
            .into_iter()
            .find(|descriptor| descriptor.retrieval.action_id == "rss.discover")
            .filter(|descriptor| self.settings.source_catalog.admits(descriptor))
            .ok_or(SubscriptionMutationError::Unavailable)?;
        let source_id = format!(
            "custom-rss-{}",
            &blake3::hash(url.as_bytes()).to_hex().to_string()[..20]
        );
        let source_revision =
            blake3::hash(format!("{}:{}", custom.display_name.trim(), url).as_bytes())
                .to_hex()
                .to_string();
        Ok(ObservableSourceOffer {
            offer_id: format!("{source_id}:observe-rss"),
            source_id,
            source_revision: source_revision.clone(),
            profile_id: "observe-rss".into(),
            profile_revision: blake3::hash(format!("{source_revision}:observe-rss").as_bytes())
                .to_hex()
                .to_string(),
            display_name: custom.display_name.trim().to_string(),
            category: "custom".into(),
            description: "Custom RSS or Atom feed".into(),
            readiness: ObservableSourceReadiness::Eligible,
            unavailable_reason: None,
            subscribed: false,
            supported_cadence: vec![
                ObservationCadence::Hourly,
                ObservationCadence::TwiceDaily,
                ObservationCadence::Daily,
            ],
            default_cadence: ObservationCadence::Hourly,
            limits: super::ObservationProfileLimits::default(),
            targets: vec![url],
            action_bindings: vec![ObservableActionBinding {
                action_id: "rss.discover".into(),
                adapter_id: descriptor.adapter_id,
            }],
        })
    }

    fn catalog(&self, principal: &str, workspace: &str) -> ObservableCatalog {
        let mut roots = vec![self.workspace.scope_skills_root(principal, workspace)];
        roots.extend(crate::magician_v2::config_extras::extra_skills_dirs());
        roots.retain(|root| root.is_dir());
        discover_observable_sources(&roots)
    }

    async fn persist_enrichment_handoff(
        &self,
        envelope: &ObservedContentEnvelope,
    ) -> Result<bool, SubscriptionMutationError> {
        let dir = self.ingress_dir(&envelope.principal, &envelope.workspace);
        self.workspace
            .create_dir_all_path(&dir)
            .await
            .context("creating observable source enrichment ingress")?;
        let path = dir.join(format!(
            "{}--{}.json",
            envelope.subscription_id, envelope.selection.candidate_fingerprint
        ));
        if path.exists() {
            return Ok(false);
        }
        self.workspace
            .write_json_atomic_path(path, envelope)
            .await
            .context("persisting observable source enrichment handoff")?;
        tracing::debug!(
            principal = envelope.principal.as_str(),
            workspace = envelope.workspace.as_str(),
            source_id = envelope.source_id.as_str(),
            profile_id = envelope.profile_id.as_str(),
            subscription_id = envelope.subscription_id.as_str(),
            action_id = envelope.action_id.as_str(),
            receipt_id = envelope.selection.receipt_id.as_str(),
            "observable source enrichment handoff persisted"
        );
        Ok(true)
    }

    async fn drain_enrichment_ingress_once(&self) {
        if self.resurfacing_sink.is_none() {
            return;
        }
        for (principal, workspace) in self.workspace.list_tenant_scopes() {
            if let Err(error) = self.drain_scope_enrichment(&principal, &workspace).await {
                tracing::warn!(
                    principal,
                    workspace,
                    error = %error,
                    "observable source enrichment drain failed"
                );
            }
        }
    }

    async fn drain_scope_enrichment(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<usize, SubscriptionMutationError> {
        let _drain_guard = self.enrichment_lock.lock().await;
        let Some(store) = self.resurfacing_sink.as_ref() else {
            return Ok(0);
        };
        let ingress = self.ingress_dir(principal, workspace);
        let mut entries = self
            .workspace
            .read_dir_path_or_empty(&ingress)
            .await
            .context("reading observable source enrichment ingress")?;
        entries.retain(|entry| entry.is_file && entry.file_name.ends_with(".json"));
        entries.sort_by(|left, right| left.file_name.cmp(&right.file_name));

        let mut processed = 0usize;
        let mut outcomes = BTreeMap::<String, (u64, u64)>::new();
        for entry in entries.into_iter().take(MAX_ENRICHMENT_BATCH_PER_SCOPE) {
            let path = ingress.join(&entry.file_name);
            let body = match self.workspace.read_to_string_path(&path).await {
                Ok(body) => body,
                Err(error) => {
                    tracing::warn!(
                        principal,
                        workspace,
                        file = entry.file_name,
                        error = %error,
                        "observable source enrichment handoff could not be read; leaving it pending"
                    );
                    continue;
                },
            };
            let envelope = match serde_json::from_str::<ObservedContentEnvelope>(&body) {
                Ok(envelope) => envelope,
                Err(error) => {
                    self.quarantine_enrichment_handoff(
                        principal,
                        workspace,
                        &entry.file_name,
                        &path,
                        &format!("invalid envelope: {error}"),
                    )
                    .await?;
                    continue;
                },
            };
            let candidate = match observed_envelope_to_resurfacing(&envelope, principal, workspace)
            {
                Ok(candidate) => candidate,
                Err(error) => {
                    self.quarantine_enrichment_handoff(
                        principal,
                        workspace,
                        &entry.file_name,
                        &path,
                        &error.to_string(),
                    )
                    .await?;
                    let outcome = outcomes
                        .entry(envelope.subscription_id.clone())
                        .or_default();
                    outcome.1 = outcome.1.saturating_add(1);
                    continue;
                },
            };

            if let Err(error) = store
                .upsert_candidate(principal, workspace, &candidate)
                .await
            {
                tracing::warn!(
                    principal,
                    workspace,
                    subscription_id = envelope.subscription_id,
                    source_id = envelope.source_id,
                    candidate_id = candidate.candidate_id,
                    error = %error,
                    "observable source enrichment admission failed; leaving it pending"
                );
                continue;
            }
            self.workspace
                .remove_file_path(&path)
                .await
                .context("removing admitted observable source enrichment handoff")?;
            processed = processed.saturating_add(1);
            self.with_metrics(|metrics| {
                metrics.enrichment_processed = metrics.enrichment_processed.saturating_add(1)
            });
            let outcome = outcomes
                .entry(envelope.subscription_id.clone())
                .or_default();
            outcome.0 = outcome.0.saturating_add(1);
        }
        self.record_enrichment_outcomes(principal, workspace, &outcomes)
            .await;
        if processed > 0 {
            self.resurfacing_wake.wake();
            tracing::info!(
                principal,
                workspace,
                processed,
                "observable source enrichments admitted to Worth a look curation"
            );
        }
        Ok(processed)
    }

    async fn quarantine_enrichment_handoff(
        &self,
        principal: &str,
        workspace: &str,
        file_name: &str,
        source_path: &std::path::Path,
        reason: &str,
    ) -> Result<(), SubscriptionMutationError> {
        let failed_dir = self.failed_ingress_dir(principal, workspace);
        self.workspace
            .create_dir_all_path(&failed_dir)
            .await
            .context("creating failed observable source enrichment directory")?;
        let failed_path = failed_dir.join(file_name);
        if failed_path.exists() {
            self.workspace
                .remove_file_path(source_path)
                .await
                .context("removing duplicate failed observable source handoff")?;
        } else {
            self.workspace
                .rename_path_sync(source_path, &failed_path)
                .context("quarantining failed observable source enrichment handoff")?;
        }
        self.with_metrics(|metrics| {
            metrics.enrichment_failed = metrics.enrichment_failed.saturating_add(1)
        });
        tracing::warn!(
            principal,
            workspace,
            file = file_name,
            error = reason,
            "observable source enrichment handoff quarantined"
        );
        let mut failed = self
            .workspace
            .read_dir_path_or_empty(&failed_dir)
            .await
            .context("reading failed observable source handoffs for retention")?;
        failed.retain(|entry| entry.is_file && entry.file_name.ends_with(".json"));
        failed.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        let remove_count = failed.len().saturating_sub(MAX_FAILED_HANDOFFS_PER_SCOPE);
        for entry in failed.into_iter().take(remove_count) {
            self.workspace
                .remove_file_path(failed_dir.join(entry.file_name))
                .await
                .context("pruning failed observable source handoff")?;
        }
        Ok(())
    }

    async fn record_enrichment_outcomes(
        &self,
        principal: &str,
        workspace: &str,
        outcomes: &BTreeMap<String, (u64, u64)>,
    ) {
        if outcomes.is_empty() {
            return;
        }
        let scope_lock = self.scope_lock(principal, workspace);
        let _guard = scope_lock.lock().await;
        let result: Result<(), SubscriptionMutationError> = async {
            let active_subscription_ids = self
                .load_file_unlocked(principal, workspace)
                .await?
                .subscriptions
                .into_iter()
                .map(|subscription| subscription.subscription_id)
                .collect::<HashSet<_>>();
            let mut file = self
                .load_observability_file_unlocked(principal, workspace)
                .await?;
            for (subscription_id, (processed, failed)) in outcomes {
                if !active_subscription_ids.contains(subscription_id) {
                    continue;
                }
                let aggregate = file.sources.entry(subscription_id.clone()).or_default();
                aggregate.enrichment_processed =
                    aggregate.enrichment_processed.saturating_add(*processed);
                aggregate.enrichment_failed = aggregate.enrichment_failed.saturating_add(*failed);
            }
            self.save_observability_file_unlocked(principal, workspace, &file)
                .await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(
                principal,
                workspace,
                error = %error,
                "observable source enrichment outcomes could not be persisted"
            );
        }
    }

    fn ingress_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.workspace
            .scope_root(principal, workspace)
            .join(OBSERVATION_ROOT)
            .join(ENRICHMENT_INGRESS_DIR)
    }

    fn failed_ingress_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.workspace
            .scope_root(principal, workspace)
            .join(OBSERVATION_ROOT)
            .join(ENRICHMENT_FAILED_DIR)
    }

    fn subscriptions_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.workspace
            .scope_root(principal, workspace)
            .join(OBSERVATION_ROOT)
            .join(SUBSCRIPTIONS_FILE)
    }

    fn observability_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.workspace
            .scope_root(principal, workspace)
            .join(OBSERVATION_ROOT)
            .join(OBSERVABILITY_FILE)
    }

    async fn load_file(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<SubscriptionFile, SubscriptionMutationError> {
        let scope_lock = self.scope_lock(principal, workspace);
        let _guard = scope_lock.lock().await;
        self.load_file_unlocked(principal, workspace).await
    }

    async fn load_file_unlocked(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<SubscriptionFile, SubscriptionMutationError> {
        let path = self.subscriptions_path(principal, workspace);
        if !path.exists() {
            return Ok(SubscriptionFile::default());
        }
        let file = self
            .workspace
            .read_json_path::<SubscriptionFile, _>(&path)
            .await
            .context("reading observable source subscriptions")?;
        if file.schema_version != SUBSCRIPTION_SCHEMA_VERSION {
            return Err(SubscriptionMutationError::Internal(anyhow!(
                "unsupported observable source subscription schema"
            )));
        }
        for subscription in &file.subscriptions {
            subscription
                .validate_persisted(principal, workspace)
                .context("validating observable source subscription state")?;
        }
        Ok(file)
    }

    async fn save_file_unlocked(
        &self,
        principal: &str,
        workspace: &str,
        file: &SubscriptionFile,
    ) -> Result<(), SubscriptionMutationError> {
        let path = self.subscriptions_path(principal, workspace);
        if let Some(parent) = path.parent() {
            self.workspace
                .create_dir_all_path(parent)
                .await
                .context("creating observable source state directory")?;
        }
        self.workspace
            .write_json_atomic_path(path, file)
            .await
            .context("writing observable source subscriptions")?;
        Ok(())
    }

    async fn record_run_best_effort(
        &self,
        principal: &str,
        workspace: &str,
        record: ObservationRunRecord,
    ) {
        if let Err(error) = self.record_run(principal, workspace, &record).await {
            tracing::warn!(
                principal,
                workspace,
                subscription_id = record.subscription_id.as_str(),
                run_id = record.run_id.as_str(),
                error = %error,
                "observable source run observability could not be persisted"
            );
        }
    }

    async fn record_run(
        &self,
        principal: &str,
        workspace: &str,
        record: &ObservationRunRecord,
    ) -> Result<(), SubscriptionMutationError> {
        let scope_lock = self.scope_lock(principal, workspace);
        let _guard = scope_lock.lock().await;
        if !self
            .load_file_unlocked(principal, workspace)
            .await?
            .subscriptions
            .iter()
            .any(|subscription| subscription.subscription_id == record.subscription_id)
        {
            return Ok(());
        }
        let mut file = self
            .load_observability_file_unlocked(principal, workspace)
            .await?;
        let aggregate = file
            .sources
            .entry(record.subscription_id.clone())
            .or_default();
        aggregate.runs = aggregate.runs.saturating_add(1);
        match record.status {
            ObservationRunStatus::Succeeded => {
                aggregate.succeeded = aggregate.succeeded.saturating_add(1)
            },
            ObservationRunStatus::Failed => aggregate.failed = aggregate.failed.saturating_add(1),
            ObservationRunStatus::Cancelled => {
                aggregate.cancelled = aggregate.cancelled.saturating_add(1)
            },
        }
        aggregate.candidates_discovered = aggregate
            .candidates_discovered
            .saturating_add(record.discovered);
        aggregate.candidates_deduped = aggregate.candidates_deduped.saturating_add(record.deduped);
        aggregate.candidates_selected = aggregate
            .candidates_selected
            .saturating_add(record.selected);
        aggregate.enrichment_handoffs = aggregate
            .enrichment_handoffs
            .saturating_add(record.handed_off);
        if record.cursor_advanced {
            aggregate.cursor_advances = aggregate.cursor_advances.saturating_add(1);
        }
        aggregate.modified_targets = aggregate
            .modified_targets
            .saturating_add(record.modified_targets);
        aggregate.not_modified_targets = aggregate
            .not_modified_targets
            .saturating_add(record.not_modified_targets);
        aggregate.response_bytes = aggregate
            .response_bytes
            .saturating_add(record.response_bytes);
        aggregate.total_latency_ms = aggregate
            .total_latency_ms
            .saturating_add(record.duration_ms);
        if let Some(cost) = record.cost.as_ref() {
            let total = aggregate
                .cost_microunits
                .entry(cost.commodity.clone())
                .or_default();
            *total = total.saturating_add(cost.amount_microunits);
        }
        aggregate.last_run = Some(record.clone());
        file.recent_runs.insert(0, record.clone());
        file.recent_runs.truncate(MAX_OBSERVATION_RUN_HISTORY);
        self.save_observability_file_unlocked(principal, workspace, &file)
            .await
    }

    async fn load_observability_file_unlocked(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<ObservationObservabilityFile, SubscriptionMutationError> {
        let path = self.observability_path(principal, workspace);
        if !path.exists() {
            return Ok(ObservationObservabilityFile::default());
        }
        let file = self
            .workspace
            .read_json_path::<ObservationObservabilityFile, _>(&path)
            .await
            .context("reading observable source observability")?;
        validate_observability_file(&file)?;
        Ok(file)
    }

    async fn save_observability_file_unlocked(
        &self,
        principal: &str,
        workspace: &str,
        file: &ObservationObservabilityFile,
    ) -> Result<(), SubscriptionMutationError> {
        validate_observability_file(file)?;
        let path = self.observability_path(principal, workspace);
        if let Some(parent) = path.parent() {
            self.workspace
                .create_dir_all_path(parent)
                .await
                .context("creating observable source observability directory")?;
        }
        self.workspace
            .write_json_atomic_path(path, file)
            .await
            .context("writing observable source observability")?;
        Ok(())
    }

    fn scope_lock(&self, principal: &str, workspace: &str) -> Arc<AsyncMutex<()>> {
        self.scope_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry((principal.to_string(), workspace.to_string()))
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn with_metrics(&self, update: impl FnOnce(&mut ObservableSourceMetricsSnapshot)) {
        let mut metrics = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut metrics);
    }
}

struct ExecutedObservation {
    outcome: ObservationRunOutcome,
    handoffs: Vec<ObservedContentEnvelope>,
    next_validators: BTreeMap<String, DiscoveryValidator>,
}

#[derive(Debug, Clone, Copy)]
struct AutomaticObservationPolicy {
    max_candidates: usize,
    retain_from_ms: i64,
    ignore_cursor: bool,
    discard_next_cursor: bool,
}

fn observation_run_record(
    subscription: &ObservationSubscription,
    trigger: ObservationRunTrigger,
    status: ObservationRunStatus,
    duration_ms: u64,
    outcome: Option<&ObservationRunOutcome>,
    error_class: Option<&str>,
) -> ObservationRunRecord {
    let finished_at_ms = Utc::now().timestamp_millis();
    let duration_i64 = duration_ms.min(i64::MAX as u64) as i64;
    ObservationRunRecord {
        schema_version: SUBSCRIPTION_SCHEMA_VERSION,
        run_id: Uuid::new_v4().to_string(),
        subscription_id: subscription.subscription_id.clone(),
        source_id: subscription.source_id.clone(),
        profile_id: subscription.profile_id.clone(),
        action_id: outcome
            .map(|value| value.action_id.clone())
            .or_else(|| {
                subscription
                    .action_bindings
                    .first()
                    .map(|binding| binding.action_id.clone())
            })
            .unwrap_or_default(),
        trigger,
        status,
        started_at_ms: subscription
            .last_run_started_at_ms
            .unwrap_or_else(|| finished_at_ms.saturating_sub(duration_i64)),
        finished_at_ms,
        duration_ms,
        discovered: outcome.map_or(0, |value| value.discovered as u64),
        deduped: outcome.map_or(0, |value| value.deduped as u64),
        selected: outcome.map_or(0, |value| value.selected as u64),
        handed_off: outcome.map_or(0, |value| value.handed_off as u64),
        cursor_advanced: outcome.is_some_and(|value| value.cursor_advanced),
        modified_targets: outcome.map_or(0, |value| value.transport.modified_targets),
        not_modified_targets: outcome.map_or(0, |value| value.transport.not_modified_targets),
        response_bytes: outcome.map_or(0, |value| value.transport.response_bytes),
        error_class: error_class.map(str::to_string),
        cost: outcome.and_then(|value| value.cost.clone()),
    }
}

fn observation_totals(
    items: &[ObservationSourceObservabilitySummary],
) -> ObservationObservabilityTotals {
    let mut totals = ObservationObservabilityTotals::default();
    totals.subscriptions = items.len() as u64;
    for item in items {
        if item.enabled {
            totals.enabled = totals.enabled.saturating_add(1);
        }
        match item.last_run.as_ref().map(|run| run.status) {
            None => totals.never_run = totals.never_run.saturating_add(1),
            Some(ObservationRunStatus::Succeeded) if item.enabled => {
                totals.healthy = totals.healthy.saturating_add(1)
            },
            Some(ObservationRunStatus::Failed | ObservationRunStatus::Cancelled)
                if item.enabled =>
            {
                totals.degraded = totals.degraded.saturating_add(1)
            },
            Some(_) => {},
        }
        totals.runs = totals.runs.saturating_add(item.runs);
        totals.succeeded = totals.succeeded.saturating_add(item.succeeded);
        totals.failed = totals.failed.saturating_add(item.failed);
        totals.cancelled = totals.cancelled.saturating_add(item.cancelled);
        totals.candidates_discovered = totals
            .candidates_discovered
            .saturating_add(item.candidates_discovered);
        totals.candidates_deduped = totals
            .candidates_deduped
            .saturating_add(item.candidates_deduped);
        totals.candidates_selected = totals
            .candidates_selected
            .saturating_add(item.candidates_selected);
        totals.enrichment_handoffs = totals
            .enrichment_handoffs
            .saturating_add(item.enrichment_handoffs);
        totals.enrichment_processed = totals
            .enrichment_processed
            .saturating_add(item.enrichment_processed);
        totals.enrichment_failed = totals
            .enrichment_failed
            .saturating_add(item.enrichment_failed);
        totals.modified_targets = totals
            .modified_targets
            .saturating_add(item.modified_targets);
        totals.not_modified_targets = totals
            .not_modified_targets
            .saturating_add(item.not_modified_targets);
        totals.response_bytes = totals.response_bytes.saturating_add(item.response_bytes);
        totals.total_latency_ms = totals
            .total_latency_ms
            .saturating_add(item.total_latency_ms);
        for (commodity, amount) in &item.cost_microunits {
            let total = totals.cost_microunits.entry(commodity.clone()).or_default();
            *total = total.saturating_add(*amount);
        }
    }
    totals
}

fn validate_observability_file(
    file: &ObservationObservabilityFile,
) -> Result<(), SubscriptionMutationError> {
    if file.schema_version != SUBSCRIPTION_SCHEMA_VERSION
        || file.sources.len() > MAX_OBSERVATION_SOURCE_AGGREGATES
        || file.recent_runs.len() > MAX_OBSERVATION_RUN_HISTORY
    {
        return Err(SubscriptionMutationError::Internal(anyhow!(
            "observable source observability state exceeds schema bounds"
        )));
    }
    for (subscription_id, aggregate) in &file.sources {
        validate_observability_label(subscription_id, "subscription id", 160)?;
        if aggregate
            .cost_microunits
            .keys()
            .any(|commodity| validate_observability_label(commodity, "cost commodity", 64).is_err())
        {
            return Err(SubscriptionMutationError::Internal(anyhow!(
                "observable source observability cost is invalid"
            )));
        }
        if let Some(last_run) = aggregate.last_run.as_ref() {
            validate_run_record(last_run)?;
        }
    }
    for record in &file.recent_runs {
        validate_run_record(record)?;
    }
    Ok(())
}

fn validate_run_record(record: &ObservationRunRecord) -> Result<(), SubscriptionMutationError> {
    if record.schema_version != SUBSCRIPTION_SCHEMA_VERSION
        || record.finished_at_ms < record.started_at_ms
    {
        return Err(SubscriptionMutationError::Internal(anyhow!(
            "observable source run record is invalid"
        )));
    }
    for (value, label, maximum) in [
        (record.run_id.as_str(), "run id", 64usize),
        (record.subscription_id.as_str(), "subscription id", 160usize),
        (record.source_id.as_str(), "source id", 128usize),
        (record.profile_id.as_str(), "profile id", 128usize),
        (record.action_id.as_str(), "action id", 128usize),
    ] {
        validate_observability_label(value, label, maximum)?;
    }
    if record
        .error_class
        .as_deref()
        .is_some_and(|value| validate_observability_label(value, "error class", 128).is_err())
        || record
            .cost
            .as_ref()
            .is_some_and(|cost| cost.validate().is_err())
    {
        return Err(SubscriptionMutationError::Internal(anyhow!(
            "observable source run metadata is invalid"
        )));
    }
    Ok(())
}

fn validate_observability_label(
    value: &str,
    label: &str,
    maximum: usize,
) -> Result<(), SubscriptionMutationError> {
    if value.trim().is_empty()
        || value.chars().count() > maximum
        || value.chars().any(char::is_control)
    {
        return Err(SubscriptionMutationError::Internal(anyhow!(
            "observable source {label} is invalid"
        )));
    }
    Ok(())
}

#[derive(Debug)]
struct SelectedCandidate {
    fingerprint: String,
    candidate: ContentCandidate,
    relevance_score: Option<f64>,
}

fn observed_envelope_to_resurfacing(
    envelope: &ObservedContentEnvelope,
    principal: &str,
    workspace: &str,
) -> Result<ResurfacingCandidate> {
    if envelope.schema_version != 1 {
        anyhow::bail!(
            "unsupported observed content envelope schema {}",
            envelope.schema_version
        );
    }
    if envelope.principal != principal || envelope.workspace != workspace {
        anyhow::bail!("observed content envelope scope does not match its queue");
    }
    if envelope.selection.subscription_id != envelope.subscription_id
        || envelope.selection.source_id != envelope.source_id
        || envelope.selection.profile_id != envelope.profile_id
        || envelope.selection.action_id != envelope.action_id
    {
        anyhow::bail!("observed content envelope receipt identity is inconsistent");
    }
    envelope.candidate.validate()?;
    let source_kind = if envelope
        .candidate
        .metadata
        .get("observation_source_kind")
        .and_then(serde_json::Value::as_str)
        == Some("note")
    {
        SourceKind::Note
    } else {
        SourceKind::Web
    };
    let source_ref = if source_kind == SourceKind::Note {
        envelope.candidate.identity.item_id.clone()
    } else {
        envelope
            .candidate
            .canonical_url
            .as_deref()
            .ok_or_else(|| anyhow!("observed web candidate has no canonical URL"))
            .and_then(canonicalize_http_url)?
    };
    let digest = bounded_chars(strip_html_to_text(&envelope.candidate.cheap_text), 4_000);
    if digest.trim().is_empty() {
        anyhow::bail!("observed web candidate has no readable summary after normalization");
    }

    let now = Utc::now().timestamp();
    let occurred_at = envelope
        .candidate
        .published_at_ms
        .unwrap_or(envelope.candidate.observed_at_ms)
        .saturating_div(1_000)
        .max(1);
    let age_days = now.saturating_sub(occurred_at).max(0) as f32 / 86_400.0;
    let recency = 0.5_f32.powf(age_days / 7.0).clamp(0.0, 1.0);
    // A subscription is explicit owner interest. An intent match strengthens
    // that signal, but even an unfiltered source is not a recency-only item.
    let source_affinity = (0.5
        + envelope
            .selection
            .relevance_score
            .unwrap_or(0.0)
            .clamp(0.0, 1.0) as f32
            * 0.5)
        .clamp(0.0, 1.0);
    let signals = SalienceSignals {
        recency,
        source_affinity,
        ..SalienceSignals::default()
    };
    let salience_score = (recency * 0.45 + source_affinity * 0.55).clamp(0.0, 1.0);

    let mut key_facts = Vec::new();
    if source_kind == SourceKind::Note {
        if let Some(path) = envelope
            .candidate
            .metadata
            .get("note_path")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            key_facts.push(bounded_chars(format!("Note: {path}"), 320));
        }
    }
    if let Some(authors) = envelope
        .candidate
        .metadata
        .get("authors")
        .and_then(serde_json::Value::as_array)
    {
        let names = authors
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .take(5)
            .collect::<Vec<_>>();
        if !names.is_empty() {
            key_facts.push(bounded_chars(format!("By {}", names.join(", ")), 320));
        }
    }
    let temporal_facts = envelope
        .candidate
        .published_at_ms
        .map(|at_ms| {
            vec![ResurfacingTemporalFact {
                kind: if source_kind == SourceKind::Note {
                    "updated".to_string()
                } else {
                    "published".to_string()
                },
                text: if source_kind == SourceKind::Note {
                    "Updated in Notes".to_string()
                } else {
                    "Published by the observed source".to_string()
                },
                at_ms: Some(at_ms),
                timezone: Some("UTC".to_string()),
            }]
        })
        .unwrap_or_default();
    let content_details = (!key_facts.is_empty() || !temporal_facts.is_empty()).then_some(
        ResurfacingContentDetails {
            schema_version: 1,
            key_facts,
            changes: Vec::new(),
            temporal_facts,
            detail_status: ResurfacingDetailStatus::Complete,
            missing_details: Vec::new(),
        },
    );
    let content_revision = envelope
        .candidate
        .content_hash
        .clone()
        .or_else(|| Some(envelope.selection.candidate_fingerprint.clone()));
    let first_seen_at = envelope.admitted_at_ms.saturating_div(1_000).max(1);

    Ok(ResurfacingCandidate {
        candidate_id: candidate_id(source_kind, &source_ref),
        source_kind,
        source_ref,
        title: envelope.candidate.title.clone(),
        content_digest: digest,
        content_details,
        content_revision,
        semantic_features: None,
        salience_score,
        signals,
        temporal_anchor_at: None,
        embedding_id: None,
        state: CandidateState::Candidate,
        first_seen_at,
        last_scored_at: now,
        last_surfaced_at: None,
        cooldown_until: 0,
        surface_count: 0,
        dismiss_count: 0,
    })
}

fn bounded_chars(value: String, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

/// Async source of the SEMANTIC side of the hybrid interest filter: given the
/// subscription `intent` and a batch of candidate `texts`, returns one raw
/// cosine similarity (intent-vs-text) per input, or `None` for any text whose
/// score is unavailable (embedder down/timed out, blank text, length mismatch).
///
/// `None` degrades that candidate to keyword-only behavior — it must never
/// abort selection or drop the whole batch. Implementors are expected to return
/// a `Vec` the SAME length as `texts` (index-aligned).
#[async_trait]
pub trait SemanticIntentScorer: Send + Sync {
    async fn cosine_scores(&self, intent: &str, texts: &[String]) -> Vec<Option<f32>>;
}

/// Deterministic no-op scorer: every text scores `None`, so the hybrid filter
/// degrades to the pre-existing keyword-only term match. The default for any
/// runtime that does not opt in and for every test that doesn't inject a
/// scorer.
struct NoSemanticIntent;

#[async_trait]
impl SemanticIntentScorer for NoSemanticIntent {
    async fn cosine_scores(&self, _intent: &str, texts: &[String]) -> Vec<Option<f32>> {
        vec![None; texts.len()]
    }
}

/// Embedding-backed semantic intent scorer over the shared Ollama embedder
/// (mirrors the comms crate's EmbeddingCentrality).
/// Each call embeds the `intent` once (`embed_query`) and the candidate `texts`
/// in one batch (`embed_documents`), then cosines each text vector against the
/// intent vector using the shared normalize/cosine helpers.
///
/// Any error or timeout — on either embed — degrades the WHOLE batch to `None`
/// (keyword-only). Selection never panics and never drops everything.
struct EmbeddingIntentScorer {
    embedder: OllamaEmbedder,
    query_timeout: Duration,
}

impl EmbeddingIntentScorer {
    fn new(embedder: OllamaEmbedder, query_timeout: Duration) -> Self {
        Self {
            embedder,
            query_timeout,
        }
    }
}

#[async_trait]
impl SemanticIntentScorer for EmbeddingIntentScorer {
    async fn cosine_scores(&self, intent: &str, texts: &[String]) -> Vec<Option<f32>> {
        let none_batch = || vec![None; texts.len()];
        let intent = intent.trim();
        if intent.is_empty() || texts.is_empty() {
            return none_batch();
        }
        // Embed the intent and the candidate texts under one shared budget. Any
        // failure/timeout on either embed degrades the whole batch to None so
        // the caller falls back to keyword-only.
        let embed_both = async {
            let intent_vec = self.embedder.embed_query(intent).await?;
            let doc_vecs = self.embedder.embed_documents(texts).await?;
            Ok::<_, anyhow::Error>((intent_vec, doc_vecs))
        };
        let (intent_vec, doc_vecs) = match timeout(self.query_timeout, embed_both).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(_)) | Err(_) => return none_batch(),
        };
        if doc_vecs.len() != texts.len() {
            // Defensive: never mis-align scores with candidates.
            return none_batch();
        }
        let intent_norm = normalize(intent_vec);
        doc_vecs
            .into_iter()
            .map(|vec| Some(cosine(&intent_norm, &normalize(vec))))
            .collect()
    }
}

/// Hybrid interest filter = keyword (term overlap) OR semantic (embedding
/// cosine) match. A candidate survives when it shares a keyword with the intent
/// OR its clamped cosine is at/above the semantic floor; ranking fuses both
/// signals.
///
/// Empty intent preserves the legacy behavior EXACTLY: keep everything with a
/// `None` relevance score (no filtering, no embedding call).
async fn select_candidates(
    candidates: BTreeMap<String, ContentCandidate>,
    intent: Option<&str>,
    limit: usize,
    scorer: &dyn SemanticIntentScorer,
) -> Vec<SelectedCandidate> {
    let intent_str = intent.unwrap_or_default();
    let intent_terms = terms(intent_str);

    // Empty-intent path: keep all, score None, no filtering or embedding.
    if intent_terms.is_empty() {
        let mut selected = candidates
            .into_iter()
            .map(|(fingerprint, candidate)| SelectedCandidate {
                fingerprint,
                candidate,
                relevance_score: None,
            })
            .collect::<Vec<_>>();
        sort_selected(&mut selected);
        selected.truncate(limit);
        return selected;
    }

    // Preserve BTreeMap order into a stable index so semantic scores align with
    // the candidates they were computed for.
    let entries: Vec<(String, ContentCandidate)> = candidates.into_iter().collect();
    let texts: Vec<String> = entries
        .iter()
        .map(|(_, candidate)| format!("{} {}", candidate.title, candidate.cheap_text))
        .collect();
    let semantic = scorer.cosine_scores(intent_str, &texts).await;
    let semantic_min = semantic_intent_min();
    let total_terms = intent_terms.len() as f64;

    let mut selected = entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, (fingerprint, candidate))| {
            let haystack = terms(&format!("{} {}", candidate.title, candidate.cheap_text));
            let matched = intent_terms.intersection(&haystack).count();
            let keyword_score = matched as f64 / total_terms;

            // Clamp cosine into [0,1]; anti-correlated/negative similarity is
            // treated as "no semantic signal".
            let sem_norm = semantic
                .get(index)
                .copied()
                .flatten()
                .map(|cos| cos.clamp(0.0, 1.0));

            let keyword_hit = matched > 0;
            let semantic_hit = sem_norm.is_some_and(|value| value >= semantic_min);
            if !keyword_hit && !semantic_hit {
                return None;
            }

            // Fuse when a semantic score exists; otherwise fall back to the
            // keyword score alone (identical to the legacy behavior).
            let relevance = match sem_norm {
                Some(sem) => 0.6 * sem as f64 + 0.4 * keyword_score,
                None => keyword_score,
            };
            Some(SelectedCandidate {
                fingerprint,
                candidate,
                relevance_score: Some(relevance),
            })
        })
        .collect::<Vec<_>>();
    sort_selected(&mut selected);
    selected.truncate(limit);
    selected
}

/// Deterministic ranking shared by both intent paths: relevance score desc,
/// then published-at desc, then fingerprint asc (stable tie-break).
fn sort_selected(selected: &mut [SelectedCandidate]) {
    selected.sort_by(|a, b| {
        b.relevance_score
            .partial_cmp(&a.relevance_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.candidate
                    .published_at_ms
                    .cmp(&a.candidate.published_at_ms)
            })
            .then_with(|| a.fingerprint.cmp(&b.fingerprint))
    });
}

fn terms(value: &str) -> BTreeSet<String> {
    value
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|term| term.chars().count() >= 2)
        .map(str::to_lowercase)
        .collect()
}

fn candidate_fingerprint(candidate: &ContentCandidate) -> String {
    if candidate
        .metadata
        .get("observation_source_kind")
        .and_then(serde_json::Value::as_str)
        == Some("note")
    {
        return blake3::hash(
            format!(
                "{}\0{}",
                candidate.identity.item_id,
                candidate.content_hash.as_deref().unwrap_or_default()
            )
            .as_bytes(),
        )
        .to_hex()
        .to_string();
    }
    let identity = candidate
        .canonical_url
        .as_deref()
        .or(candidate.content_hash.as_deref())
        .unwrap_or(&candidate.identity.item_id);
    blake3::hash(identity.as_bytes()).to_hex().to_string()
}

fn subscription_id(
    principal: &str,
    workspace: &str,
    source_id: &str,
    profile_id: &str,
    target: Option<&str>,
) -> String {
    format!(
        "obs_{}",
        &blake3::hash(
            format!(
                "{principal}\0{workspace}\0{source_id}\0{profile_id}\0{}",
                target.unwrap_or_default()
            )
            .as_bytes()
        )
        .to_hex()
        .to_string()[..32]
    )
}

fn paginate_by_key<T: Serialize>(
    items: Vec<T>,
    cursor: Option<&str>,
    limit: usize,
    key: impl Fn(&T) -> &str,
) -> Result<(Vec<T>, Option<String>), SubscriptionMutationError> {
    let revision = blake3::hash(
        &serde_json::to_vec(&items).context("serializing observable source pagination snapshot")?,
    )
    .to_hex()
    .to_string();
    let start = match cursor {
        Some(cursor) => {
            let cursor_key = cursor
                .strip_prefix(&revision)
                .and_then(|value| value.strip_prefix(':'))
                .ok_or(SubscriptionMutationError::StaleCursor)?;
            items
                .iter()
                .position(|item| key(item) == cursor_key)
                .map(|index| index.saturating_add(1))
                .ok_or(SubscriptionMutationError::StaleCursor)?
        },
        None => 0,
    };
    let mut page = items
        .into_iter()
        .skip(start)
        .take(limit + 1)
        .collect::<Vec<_>>();
    let has_more = page.len() > limit;
    if has_more {
        page.pop();
    }
    let next = has_more
        .then(|| page.last().map(|item| format!("{revision}:{}", key(item))))
        .flatten();
    Ok((page, next))
}

fn validate_page(limit: usize) -> Result<(), SubscriptionMutationError> {
    if limit == 0 || limit > MAX_PAGE_SIZE {
        return Err(SubscriptionMutationError::Invalid(format!(
            "limit must be between 1 and {MAX_PAGE_SIZE}"
        )));
    }
    Ok(())
}

fn normalize_intent(intent: Option<String>) -> Result<Option<String>, SubscriptionMutationError> {
    let intent = intent
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if intent.as_deref().is_some_and(|value| {
        value.chars().count() > MAX_INTENT_CHARS || value.chars().any(char::is_control)
    }) {
        return Err(SubscriptionMutationError::Invalid(
            "intent exceeds bounds or contains controls".into(),
        ));
    }
    Ok(intent)
}

fn validate_custom_name(value: &str) -> Result<(), SubscriptionMutationError> {
    if value.trim().is_empty()
        || value.trim().chars().count() > MAX_CUSTOM_NAME_CHARS
        || value.chars().any(char::is_control)
    {
        return Err(SubscriptionMutationError::Invalid(
            "custom RSS name is invalid".into(),
        ));
    }
    Ok(())
}

fn is_notes_binding(binding: Option<&ObservableActionBinding>) -> bool {
    binding.is_some_and(|binding| {
        binding.action_id == NOTES_ACTION_ID && binding.adapter_id == NOTES_ADAPTER_ID
    })
}

fn error_class(error: &SubscriptionMutationError) -> &'static str {
    match error {
        SubscriptionMutationError::NotFound => "subscription_not_found",
        SubscriptionMutationError::Unavailable => "profile_unavailable",
        SubscriptionMutationError::StaleSource => "source_revision_stale",
        SubscriptionMutationError::RevisionConflict => "revision_conflict",
        SubscriptionMutationError::StaleCursor => "stale_cursor",
        SubscriptionMutationError::Invalid(_) => "invalid_subscription",
        SubscriptionMutationError::Busy => "lease_busy",
        SubscriptionMutationError::Internal(_) => "acquisition_failed",
    }
}

fn subscription_schema_version() -> u32 {
    SUBSCRIPTION_SCHEMA_VERSION
}

fn default_true() -> bool {
    true
}

fn default_supported_cadences() -> Vec<ObservationCadence> {
    vec![
        ObservationCadence::Hourly,
        ObservationCadence::TwiceDaily,
        ObservationCadence::Daily,
    ]
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{
        collections::VecDeque,
        path::Path,
        sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use tempfile::tempdir;

    use super::*;
    use crate::magician_v2::{
        content_sources::{
            AdapterAuth, AdapterExecution, ContentPrivacy, ContentProvenance,
            ContentSourceCapabilities, ContentSourceClass, ContentSourceDescriptor,
            ContentSourceRegistry, DiscoveryAdapter, DiscoveryPage, ObservableSourceManifest,
            ObservationEscalation, ObservationProfile, ObservationProfileAcquisition,
            ObservationProfileLimits, ObservationSchedule, ObservationSurface,
            RetrievalActionMetadata, RetrievalAuthority, RetrievalOperation, RetrievalRung,
            SourceIdentity, CONTENT_SOURCE_SCHEMA_VERSION,
        },
        notes::WriteNoteMarkdownRequest,
    };

    struct TestResolver {
        services: HashMap<(String, String), Arc<ContentAcquisitionService>>,
    }

    #[async_trait]
    impl ObservableContentResolver for TestResolver {
        async fn resolve(
            &self,
            principal: String,
            workspace: String,
        ) -> Result<Arc<ContentAcquisitionService>> {
            self.services
                .get(&(principal, workspace))
                .cloned()
                .ok_or_else(|| anyhow!("test scope is not registered"))
        }
    }

    struct FakeRssAdapter {
        descriptor: ContentSourceDescriptor,
        calls: AtomicUsize,
        delay_ms: AtomicU64,
        fail: AtomicBool,
        requests: Mutex<VecDeque<DiscoveryRequest>>,
        candidates: Vec<ContentCandidate>,
    }

    impl FakeRssAdapter {
        fn new(candidates: Vec<ContentCandidate>, cursor: bool) -> Self {
            let mut retrieval = RetrievalActionMetadata::discovery(
                "rss.discover",
                RetrievalRung::SourceNative,
                RetrievalAuthority::PublicRemoteRead,
                true,
            );
            retrieval.accepts_targets = true;
            retrieval.requires_targets = true;
            Self {
                descriptor: ContentSourceDescriptor {
                    adapter_id: "rss".into(),
                    display_name: "Fixture RSS".into(),
                    class: ContentSourceClass::Syndication,
                    capabilities: ContentSourceCapabilities {
                        discovery: true,
                        full_content: false,
                        cursor,
                        conditional_fetch: true,
                        execution: AdapterExecution::RemoteEndpoint,
                        auth: AdapterAuth::None,
                        sends_user_intent: false,
                        metered: false,
                    },
                    retrieval,
                },
                calls: AtomicUsize::new(0),
                delay_ms: AtomicU64::new(0),
                fail: AtomicBool::new(false),
                requests: Mutex::new(VecDeque::new()),
                candidates,
            }
        }

        fn requests(&self) -> Vec<DiscoveryRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .cloned()
                .collect()
        }
    }

    #[async_trait]
    impl DiscoveryAdapter for FakeRssAdapter {
        fn descriptor(&self) -> &ContentSourceDescriptor {
            &self.descriptor
        }

        async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryPage> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push_back(request.clone());
            let delay_ms = self.delay_ms.load(Ordering::SeqCst);
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            if self.fail.load(Ordering::SeqCst) {
                anyhow::bail!("fixture RSS failed");
            }
            let sequence = self.calls.load(Ordering::SeqCst);
            let validators = request
                .targets
                .iter()
                .map(|target| {
                    (
                        target.clone(),
                        DiscoveryValidator {
                            etag: Some(format!("fixture-v{sequence}")),
                            last_modified: None,
                        },
                    )
                })
                .collect();
            Ok(DiscoveryPage {
                items: self
                    .candidates
                    .iter()
                    .take(request.limit)
                    .cloned()
                    .collect(),
                next_cursor: self
                    .descriptor
                    .capabilities
                    .cursor
                    .then(|| format!("cursor-{sequence}")),
                validators,
                cost: None,
                transport: DiscoveryTransportStats {
                    modified_targets: 1,
                    not_modified_targets: 0,
                    response_bytes: 512,
                },
            })
        }
    }

    fn source_manifest(source_id: &str, action: &str) -> super::super::ObservableSourceManifest {
        super::super::ObservableSourceManifest {
            schema_version: 1,
            source: super::super::ObservableSourceDefinition {
                id: source_id.into(),
                display_name: "Fixture source".into(),
                category: "research".into(),
                description: "A deterministic fixture source".into(),
            },
            profiles: vec![ObservationProfile {
                id: "observe-rss".into(),
                surfaces: vec![ObservationSurface::Observe],
                discoverable: true,
                unattended: true,
                read_only: true,
                operation: RetrievalOperation::Discover,
                acquisition: ObservationProfileAcquisition {
                    allowed_actions: vec![action.into()],
                    escalation: ObservationEscalation::None,
                    targets: vec!["https://example.com/feed.xml".into()],
                    ladder: None,
                    maximum_authority: Some(RetrievalAuthority::PublicRemoteRead),
                },
                schedule: ObservationSchedule {
                    default: ObservationCadence::Hourly,
                    allowed: vec![ObservationCadence::Hourly, ObservationCadence::Daily],
                },
                limits: ObservationProfileLimits {
                    max_candidates_per_run: 20,
                    max_selected_per_run: 10,
                },
            }],
        }
    }

    fn write_source_manifest(
        workspace: &ArtifactV2Workspace,
        principal: &str,
        scope: &str,
        source_id: &str,
        action: &str,
    ) {
        let dir = workspace
            .scope_skills_root(principal, scope)
            .join(source_id);
        write_source_skill(&dir, source_id, &source_manifest(source_id, action));
    }

    fn write_source_skill(dir: &Path, skill_name: &str, manifest: &ObservableSourceManifest) {
        std::fs::create_dir_all(&dir).unwrap();
        let extension = serde_yaml::to_string(manifest)
            .unwrap()
            .lines()
            .map(|line| format!("      {line}\n"))
            .collect::<String>();
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {skill_name}\ndescription: observable fixture\nmetadata:\n  \
                 magician:\n    observe_source:\n{extension}---\nFixture.\n"
            ),
        )
        .unwrap();
    }

    fn runtime_with_adapter(
        root: &std::path::Path,
        adapter: Arc<FakeRssAdapter>,
        scopes: &[(&str, &str)],
    ) -> Arc<ObservableSourceRuntime> {
        runtime_with_adapter_and_settings(
            root,
            adapter,
            scopes,
            ObservableSourceSettings::default(),
        )
    }

    fn runtime_with_adapter_and_settings(
        root: &std::path::Path,
        adapter: Arc<FakeRssAdapter>,
        scopes: &[(&str, &str)],
        settings: ObservableSourceSettings,
    ) -> Arc<ObservableSourceRuntime> {
        runtime_with_adapter_settings_and_sink(root, adapter, scopes, settings, None)
    }

    fn runtime_with_adapter_settings_and_sink(
        root: &std::path::Path,
        adapter: Arc<FakeRssAdapter>,
        scopes: &[(&str, &str)],
        settings: ObservableSourceSettings,
        resurfacing_sink: Option<Arc<dyn ResurfacingSink>>,
    ) -> Arc<ObservableSourceRuntime> {
        let workspace = ArtifactV2Workspace::new(root);
        let mut services = HashMap::new();
        for (principal, scope) in scopes {
            write_source_manifest(
                &workspace,
                principal,
                scope,
                "fixture-source",
                "rss.discover",
            );
            let mut registry = ContentSourceRegistry::new();
            let discovery_adapter: Arc<dyn DiscoveryAdapter> = adapter.clone();
            registry.register_discovery(discovery_adapter).unwrap();
            let service = ContentAcquisitionService::new(
                *principal,
                *scope,
                "fixture-capabilities",
                registry,
                Vec::new(),
            )
            .unwrap();
            services.insert(
                ((*principal).to_string(), (*scope).to_string()),
                Arc::new(service),
            );
        }
        let runtime = ObservableSourceRuntime::new_with_resolver(
            workspace,
            Arc::new(TestResolver { services }),
            settings,
        )
        .unwrap();
        Arc::new(match resurfacing_sink {
            Some(store) => runtime.with_resurfacing_sink(store, ResurfacingWakeHandle::default()),
            None => runtime,
        })
    }

    fn put_input(expected_revision: Option<u64>) -> PutObservationSubscription {
        PutObservationSubscription {
            source_id: Some("fixture-source".into()),
            profile_id: Some("observe-rss".into()),
            source_revision: None,
            expected_revision,
            enabled: true,
            cadence: ObservationCadence::Hourly,
            intent: Some("AI research".into()),
            max_candidates_per_run: Some(20),
            max_selected_per_run: Some(10),
            custom_rss: None,
        }
    }

    async fn make_subscription_due(runtime: &ObservableSourceRuntime, subscription_id: &str) {
        let scope_lock = runtime.scope_lock("owner", "default");
        let _guard = scope_lock.lock().await;
        let mut file = runtime
            .load_file_unlocked("owner", "default")
            .await
            .unwrap();
        let subscription = file
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.subscription_id == subscription_id)
            .unwrap();
        subscription.next_run_at_ms = 0;
        runtime
            .save_file_unlocked("owner", "default", &file)
            .await
            .unwrap();
    }

    fn candidate(item: &str, url: &str, text: &str) -> ContentCandidate {
        ContentCandidate {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: SourceIdentity::new("rss", item).unwrap(),
            title: item.into(),
            cheap_text: text.into(),
            canonical_url: Some(url.into()),
            published_at_ms: Some(100),
            observed_at_ms: 200,
            privacy: ContentPrivacy::Public,
            content_hash: None,
            provenance: ContentProvenance {
                source_label: "fixture".into(),
                source_url: Some("https://example.com/feed".into()),
                retrieved_by: "rss".into(),
            },
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn canonical_url_dedup_beats_changed_feed_guid() {
        let first = candidate("guid-1", "https://example.com/a", "AI launch");
        let second = candidate("guid-2", "https://example.com/a", "AI launch changed");
        let mut unique = BTreeMap::new();
        unique.insert(candidate_fingerprint(&first), first);
        unique
            .entry(candidate_fingerprint(&second))
            .or_insert(second);
        assert_eq!(unique.len(), 1);
    }

    /// A [`SemanticIntentScorer`] that returns fixed, per-text cosines keyed by
    /// the candidate `title` (which the runtime prefixes onto each text), so
    /// tests can pin the semantic side deterministically without an embedder.
    struct FakeSemanticIntent {
        by_title: std::collections::HashMap<String, f32>,
    }

    impl FakeSemanticIntent {
        fn new(scores: &[(&str, f32)]) -> Self {
            Self {
                by_title: scores
                    .iter()
                    .map(|(title, score)| ((*title).to_string(), *score))
                    .collect(),
            }
        }
    }

    #[async_trait]
    impl SemanticIntentScorer for FakeSemanticIntent {
        async fn cosine_scores(&self, _intent: &str, texts: &[String]) -> Vec<Option<f32>> {
            texts
                .iter()
                .map(|text| {
                    self.by_title
                        .iter()
                        .find(|(title, _)| text.starts_with(title.as_str()))
                        .map(|(_, score)| *score)
                })
                .collect()
        }
    }

    fn candidates_map(items: Vec<ContentCandidate>) -> BTreeMap<String, ContentCandidate> {
        let mut map = BTreeMap::new();
        for value in items {
            map.insert(candidate_fingerprint(&value), value);
        }
        map
    }

    #[tokio::test]
    async fn intent_selection_is_bounded_and_deterministic() {
        let candidates = candidates_map(vec![
            candidate("a", "https://example.com/a", "AI developer tool"),
            candidate("b", "https://example.com/b", "gardening"),
            candidate("c", "https://example.com/c", "AI research"),
        ]);
        let selected = select_candidates(candidates, Some("AI tools"), 1, &NoSemanticIntent).await;
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].candidate.identity.item_id, "a");
    }

    #[tokio::test]
    async fn semantic_only_match_is_kept_without_keyword_overlap() {
        // "solar" shares no keyword with intent "renewable energy", but the fake
        // scores it above the floor → it must survive on the semantic side alone.
        let candidates = candidates_map(vec![candidate(
            "solar",
            "https://example.com/solar",
            "photovoltaic panels",
        )]);
        let scorer = FakeSemanticIntent::new(&[("solar", 0.8)]);
        let selected = select_candidates(candidates, Some("renewable energy"), 10, &scorer).await;
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].candidate.identity.item_id, "solar");
    }

    #[tokio::test]
    async fn keyword_only_match_is_kept_when_semantic_absent() {
        // No semantic signal (None), but a keyword overlaps → kept, scored by
        // keyword alone (legacy degradation).
        let candidates = candidates_map(vec![candidate(
            "energy",
            "https://example.com/energy",
            "renewable energy grid",
        )]);
        let selected =
            select_candidates(candidates, Some("renewable energy"), 10, &NoSemanticIntent).await;
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].candidate.identity.item_id, "energy");
    }

    #[tokio::test]
    async fn item_with_neither_keyword_nor_semantic_is_dropped() {
        // No keyword overlap AND a below-floor cosine → dropped.
        let candidates = candidates_map(vec![candidate(
            "gardening",
            "https://example.com/gardening",
            "compost bins",
        )]);
        let scorer = FakeSemanticIntent::new(&[("gardening", 0.2)]);
        let selected = select_candidates(candidates, Some("renewable energy"), 10, &scorer).await;
        assert!(selected.is_empty());
    }

    #[tokio::test]
    async fn empty_intent_keeps_all_with_no_score() {
        let candidates = candidates_map(vec![
            candidate("a", "https://example.com/a", "anything"),
            candidate("b", "https://example.com/b", "whatever"),
        ]);
        // Empty intent must not filter or embed; scores stay None even if a
        // scorer would have returned values.
        let scorer = FakeSemanticIntent::new(&[("a", 0.9), ("b", 0.9)]);
        let selected = select_candidates(candidates, Some("   "), 10, &scorer).await;
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|s| s.relevance_score.is_none()));
    }

    #[tokio::test]
    async fn ranking_orders_by_fused_score() {
        // Both share the keyword "energy" (keyword_score 0.5 each), but "wind"
        // has a much higher cosine → higher fused score → ranks first.
        let candidates = candidates_map(vec![
            candidate(
                "wind",
                "https://example.com/wind",
                "renewable turbines energy",
            ),
            candidate("coal", "https://example.com/coal", "fossil energy plant"),
        ]);
        let scorer = FakeSemanticIntent::new(&[("wind", 0.95), ("coal", 0.6)]);
        let selected = select_candidates(candidates, Some("renewable energy"), 10, &scorer).await;
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].candidate.identity.item_id, "wind");
        assert_eq!(selected[1].candidate.identity.item_id, "coal");
        assert!(
            selected[0].relevance_score.unwrap() > selected[1].relevance_score.unwrap(),
            "wind should outrank coal on the fused score"
        );
    }

    #[test]
    fn cursor_pagination_reports_only_a_real_next_page() {
        let items = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let (first, cursor) = paginate_by_key(items.clone(), None, 2, String::as_str).unwrap();
        assert_eq!(first, vec!["a", "b"]);
        assert!(cursor.as_deref().is_some_and(|value| value.ends_with(":b")));
        let (last, cursor) = paginate_by_key(items, cursor.as_deref(), 2, String::as_str).unwrap();
        assert_eq!(last, vec!["c"]);
        assert_eq!(cursor, None);
    }

    #[test]
    fn stale_pagination_cursor_fails_instead_of_restarting() {
        let error = paginate_by_key(
            vec!["a".to_string(), "b".to_string()],
            Some("missing"),
            1,
            String::as_str,
        )
        .unwrap_err();
        assert!(matches!(error, SubscriptionMutationError::StaleCursor));
    }

    #[test]
    fn cursor_is_invalidated_when_the_ordered_collection_changes() {
        let (_, cursor) = paginate_by_key(
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            None,
            2,
            String::as_str,
        )
        .unwrap();

        let error = paginate_by_key(
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
            ],
            cursor.as_deref(),
            2,
            String::as_str,
        )
        .unwrap_err();

        assert!(matches!(error, SubscriptionMutationError::StaleCursor));
    }

    #[tokio::test]
    async fn subscription_mutations_are_scoped_idempotent_and_revision_guarded() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(Vec::new(), false));
        let runtime = runtime_with_adapter(
            temp.path(),
            adapter,
            &[("owner", "default"), ("guest", "default")],
        );

        let created = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();
        let repeated = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();
        assert_eq!(created.subscription_id, repeated.subscription_id);
        assert_eq!(created.revision, repeated.revision);
        assert_eq!(
            runtime
                .list_subscriptions("guest", "default", None, None, None, 5)
                .await
                .unwrap()
                .total,
            0
        );

        let conflict = runtime
            .put_subscription(
                "owner",
                "default",
                &created.subscription_id,
                put_input(Some(created.revision + 10)),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            conflict,
            SubscriptionMutationError::RevisionConflict
        ));
        assert!(runtime
            .delete_subscription("owner", "default", &created.subscription_id)
            .await
            .unwrap());
        assert!(!runtime
            .delete_subscription("owner", "default", &created.subscription_id)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn legacy_lease_id_is_loaded_and_canonicalized_as_lease_owner() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(Vec::new(), false));
        let runtime = runtime_with_adapter(temp.path(), adapter, &[("owner", "default")]);
        runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();

        let path = runtime.subscriptions_path("owner", "default");
        let mut persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let subscription = persisted["subscriptions"][0].as_object_mut().unwrap();
        subscription.remove("lease_owner");
        subscription.insert(
            "lease_id".into(),
            serde_json::Value::String("legacy-runtime:lease".into()),
        );
        std::fs::write(&path, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();

        let file = runtime
            .load_file_unlocked("owner", "default")
            .await
            .unwrap();
        assert_eq!(
            file.subscriptions[0].lease_owner.as_deref(),
            Some("legacy-runtime:lease")
        );
        runtime
            .save_file_unlocked("owner", "default", &file)
            .await
            .unwrap();

        let canonical: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let subscription = canonical["subscriptions"][0].as_object().unwrap();
        assert_eq!(
            subscription
                .get("lease_owner")
                .and_then(|value| value.as_str()),
            Some("legacy-runtime:lease")
        );
        assert!(!subscription.contains_key("lease_id"));
    }

    #[tokio::test]
    async fn persisted_subscription_cannot_escape_its_scope() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(Vec::new(), false));
        let runtime = runtime_with_adapter(temp.path(), adapter, &[("owner", "default")]);
        runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();
        {
            let scope_lock = runtime.scope_lock("owner", "default");
            let _guard = scope_lock.lock().await;
            let mut file = runtime
                .load_file_unlocked("owner", "default")
                .await
                .unwrap();
            file.subscriptions[0].principal = "other-owner".into();
            runtime
                .save_file_unlocked("owner", "default", &file)
                .await
                .unwrap();
        }

        let error = runtime
            .list_subscriptions("owner", "default", None, None, None, 5)
            .await
            .unwrap_err();
        assert!(matches!(error, SubscriptionMutationError::Internal(_)));
    }

    #[tokio::test]
    async fn successful_run_commits_handoff_cursor_validators_and_exact_dedup() {
        let temp = tempdir().unwrap();
        let candidates = vec![
            candidate("guid-a", "https://example.com/a", "AI research"),
            candidate("guid-b", "https://example.com/a", "AI research duplicate"),
            candidate("guid-c", "https://example.com/c", "gardening"),
        ];
        let adapter = Arc::new(FakeRssAdapter::new(candidates, true));
        let runtime =
            runtime_with_adapter(temp.path(), Arc::clone(&adapter), &[("owner", "default")]);
        let subscription = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();

        let first = runtime
            .run_now("owner", "default", &subscription.subscription_id)
            .await
            .unwrap();
        assert_eq!(
            (
                first.discovered,
                first.deduped,
                first.selected,
                first.handed_off
            ),
            (3, 1, 1, 1)
        );
        let after_first = runtime
            .list_subscriptions("owner", "default", None, None, None, 5)
            .await
            .unwrap()
            .items
            .remove(0);
        assert_eq!(after_first.cursor.as_deref(), Some("cursor-1"));
        assert_eq!(
            after_first.validators["https://example.com/feed.xml"]
                .etag
                .as_deref(),
            Some("fixture-v1")
        );
        assert_eq!(after_first.seen_fingerprints.len(), 1);

        {
            let scope_lock = runtime.scope_lock("owner", "default");
            let _guard = scope_lock.lock().await;
            let mut file = runtime
                .load_file_unlocked("owner", "default")
                .await
                .unwrap();
            file.subscriptions[0].next_run_at_ms = 0;
            runtime
                .save_file_unlocked("owner", "default", &file)
                .await
                .unwrap();
        }
        let second = runtime
            .run_subscription("owner", "default", &subscription.subscription_id, false)
            .await
            .unwrap();
        assert_eq!((second.selected, second.handed_off), (0, 0));
        let requests = adapter.requests();
        assert_eq!(
            requests[0].invocation_source,
            ContentInvocationSource::ObservedSource
        );
        assert_eq!(requests[1].cursor.as_deref(), Some("cursor-1"));
        assert_eq!(
            requests[1].validators["https://example.com/feed.xml"]
                .etag
                .as_deref(),
            Some("fixture-v1")
        );
        assert_eq!(runtime.metrics().enrichment_handoffs, 1);
    }

    #[tokio::test]
    async fn selected_web_content_drains_into_resurfacing_and_preserves_open_url() {
        let temp = tempdir().unwrap();
        let mut selected = candidate(
            "launch",
            "https://example.com/products/useful",
            "<p>An AI research launch for focused teams.</p>",
        );
        selected
            .metadata
            .insert("authors".into(), serde_json::json!(["Ada"]));
        let adapter = Arc::new(FakeRssAdapter::new(vec![selected], false));
        let store = crate::magician_v2::resurfacing_seam::FakeResurfacingSink::new();
        let runtime = runtime_with_adapter_settings_and_sink(
            temp.path(),
            adapter,
            &[("owner", "default")],
            ObservableSourceSettings::default(),
            Some(Arc::new(store.clone())),
        );
        let subscription = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();

        let outcome = runtime
            .run_now("owner", "default", &subscription.subscription_id)
            .await
            .unwrap();

        assert_eq!(outcome.handed_off, 1);
        let source_ref = "https://example.com/products/useful";
        let admitted = store
            .get_candidate(
                "owner",
                "default",
                &candidate_id(SourceKind::Web, source_ref),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(admitted.source_kind, SourceKind::Web);
        assert_eq!(admitted.source_ref, source_ref);
        assert_eq!(
            admitted.content_digest,
            "An AI research launch for focused teams."
        );
        assert!(admitted.signals.source_affinity >= 0.5);
        assert_eq!(admitted.content_details.unwrap().key_facts, vec!["By Ada"]);
        let page = runtime
            .list_observability("owner", "default", None, 5)
            .await
            .unwrap();
        assert_eq!(page.handoff_backlog, 0);
        assert_eq!(page.failed_handoffs, 0);
        assert_eq!(page.items[0].enrichment_processed, 1);
        assert_eq!(runtime.metrics().enrichment_processed, 1);
    }

    #[tokio::test]
    async fn notes_target_reuses_observe_dedup_intent_handoff_and_resurfacing_pipeline() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(Vec::new(), false));
        let store = crate::magician_v2::resurfacing_seam::FakeResurfacingSink::new();
        let runtime = runtime_with_adapter_settings_and_sink(
            temp.path(),
            Arc::clone(&adapter),
            &[("owner", "default")],
            ObservableSourceSettings::default(),
            Some(Arc::new(store.clone())),
        );
        let notes_store = NotesSettingsStore::new(temp.path());
        notes_store
            .write_note_markdown(
                "owner",
                "default",
                WriteNoteMarkdownRequest {
                    provider: Some("local_markdown".into()),
                    target_path: "Inbox/research.md".into(),
                    markdown: "# Research note\n\nQuantum sensing milestone and next steps.".into(),
                },
            )
            .await
            .unwrap();

        let offers = runtime
            .list_offers(
                "owner",
                "default",
                None,
                Some(ObservableSourceReadiness::Eligible),
                Some(false),
                None,
                10,
            )
            .await
            .unwrap();
        let offer = offers
            .items
            .into_iter()
            .find(|offer| offer.source_id == NOTES_SOURCE_ID)
            .expect("Notes is a first-class Observe offer");
        assert_eq!(offer.action_bindings[0].action_id, NOTES_ACTION_ID);
        assert_eq!(offer.targets, vec![NOTES_TARGET]);

        let subscription = runtime
            .put_subscription(
                "owner",
                "default",
                "auto",
                PutObservationSubscription {
                    source_id: Some(NOTES_SOURCE_ID.into()),
                    profile_id: Some(NOTES_PROFILE_ID.into()),
                    source_revision: Some(offer.source_revision),
                    expected_revision: None,
                    enabled: true,
                    cadence: ObservationCadence::Hourly,
                    intent: Some("quantum sensing".into()),
                    max_candidates_per_run: Some(100),
                    max_selected_per_run: Some(20),
                    custom_rss: None,
                },
            )
            .await
            .unwrap();
        let first = runtime
            .run_now("owner", "default", &subscription.subscription_id)
            .await
            .unwrap();
        assert_eq!(
            (first.discovered, first.selected, first.handed_off),
            (1, 1, 1)
        );
        assert_eq!(first.action_id, NOTES_ACTION_ID);
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);

        let source_ref = "notes:local_markdown:Inbox/research.md";
        let first_candidate = store
            .get_candidate(
                "owner",
                "default",
                &candidate_id(SourceKind::Note, source_ref),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_candidate.source_kind, SourceKind::Note);
        assert_eq!(first_candidate.source_ref, source_ref);
        assert!(first_candidate
            .content_digest
            .contains("Quantum sensing milestone"));
        let first_revision = first_candidate.content_revision.unwrap();

        make_subscription_due(&runtime, &subscription.subscription_id).await;
        let unchanged = runtime
            .run_subscription("owner", "default", &subscription.subscription_id, false)
            .await
            .unwrap();
        assert_eq!(
            (unchanged.discovered, unchanged.deduped, unchanged.selected),
            (1, 1, 0)
        );

        notes_store
            .write_note_markdown(
                "owner",
                "default",
                WriteNoteMarkdownRequest {
                    provider: Some("local_markdown".into()),
                    target_path: "Inbox/research.md".into(),
                    markdown: "# Research note\n\nQuantum sensing milestone changed after review."
                        .into(),
                },
            )
            .await
            .unwrap();
        make_subscription_due(&runtime, &subscription.subscription_id).await;
        let edited = runtime
            .run_subscription("owner", "default", &subscription.subscription_id, false)
            .await
            .unwrap();
        assert_eq!(
            (edited.discovered, edited.deduped, edited.selected),
            (1, 0, 1)
        );
        let edited_candidate = store
            .get_candidate(
                "owner",
                "default",
                &candidate_id(SourceKind::Note, source_ref),
            )
            .await
            .unwrap()
            .unwrap();
        assert_ne!(
            edited_candidate.content_revision.as_deref(),
            Some(first_revision.as_str())
        );
        assert!(edited_candidate
            .content_digest
            .contains("changed after review"));
    }

    #[tokio::test]
    async fn malformed_enrichment_is_quarantined_instead_of_blocking_the_queue() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(Vec::new(), false));
        let store = crate::magician_v2::resurfacing_seam::FakeResurfacingSink::new();
        let runtime = runtime_with_adapter_settings_and_sink(
            temp.path(),
            adapter,
            &[("owner", "default")],
            ObservableSourceSettings::default(),
            Some(Arc::new(store)
                as Arc<
                    dyn crate::magician_v2::resurfacing_seam::ResurfacingSink,
                >),
        );
        let ingress = runtime.ingress_dir("owner", "default");
        runtime
            .workspace
            .create_dir_all_path(&ingress)
            .await
            .unwrap();
        runtime
            .workspace
            .write_path(ingress.join("broken.json"), b"not-json")
            .await
            .unwrap();

        assert_eq!(
            runtime
                .drain_scope_enrichment("owner", "default")
                .await
                .unwrap(),
            0
        );
        let page = runtime
            .list_observability("owner", "default", None, 5)
            .await
            .unwrap();
        assert_eq!(page.handoff_backlog, 0);
        assert_eq!(page.failed_handoffs, 1);
        assert_eq!(runtime.metrics().enrichment_failed, 1);
    }

    #[tokio::test]
    async fn per_source_observability_is_scoped_sanitized_and_survives_restart() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(
            vec![candidate("one", "https://example.com/one", "AI research")],
            false,
        ));
        let runtime = runtime_with_adapter(
            temp.path(),
            Arc::clone(&adapter),
            &[("owner", "default"), ("guest", "default")],
        );
        let subscription = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();
        runtime
            .run_now("owner", "default", &subscription.subscription_id)
            .await
            .unwrap();

        let page = runtime
            .list_observability("owner", "default", None, 5)
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.totals.runs, 1);
        assert_eq!(page.totals.succeeded, 1);
        assert_eq!(page.totals.modified_targets, 1);
        assert_eq!(page.totals.response_bytes, 512);
        assert_eq!(page.handoff_backlog, 1);
        assert_eq!(page.run_history_retained, 1);
        assert_eq!(page.items[0].candidates_discovered, 1);
        assert_eq!(page.items[0].enrichment_handoffs, 1);
        assert_eq!(page.runtime_metrics.runs_succeeded, 1);
        let serialized = serde_json::to_string(&page).unwrap();
        // Inspect private values rather than field-name fragments. Public
        // aggregate names such as `modified_targets` and
        // `candidates_discovered` do not reveal configured or observed data.
        for private_value in ["https://", "AI research"] {
            assert!(
                !serialized.contains(private_value),
                "leaked {private_value}"
            );
        }
        assert_eq!(
            runtime
                .list_observability("guest", "default", None, 5)
                .await
                .unwrap()
                .total,
            0
        );

        let restarted = runtime_with_adapter(
            temp.path(),
            adapter,
            &[("owner", "default"), ("guest", "default")],
        );
        let after_restart = restarted
            .list_observability("owner", "default", None, 5)
            .await
            .unwrap();
        assert_eq!(after_restart.totals.runs, 1);
        assert_eq!(after_restart.runtime_metrics.runs_succeeded, 0);
        let history = restarted
            .list_run_history("owner", "default", &subscription.subscription_id, None, 5)
            .await
            .unwrap();
        assert_eq!(history.total, 1);
        assert_eq!(history.items[0].status, ObservationRunStatus::Succeeded);
        assert_eq!(history.items[0].trigger, ObservationRunTrigger::Manual);
    }

    #[tokio::test]
    async fn source_revision_change_resets_cursor_validators_and_dedup_state() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(
            vec![candidate("one", "https://example.com/one", "AI research")],
            true,
        ));
        let runtime = runtime_with_adapter(temp.path(), adapter, &[("owner", "default")]);
        let subscription = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();
        runtime
            .run_now("owner", "default", &subscription.subscription_id)
            .await
            .unwrap();
        let before = runtime
            .list_subscriptions("owner", "default", None, None, None, 5)
            .await
            .unwrap()
            .items
            .remove(0);
        assert!(before.cursor.is_some());
        assert!(!before.validators.is_empty());
        assert!(!before.seen_fingerprints.is_empty());

        let mut changed = source_manifest("fixture-source", "rss.discover");
        changed.source.description = "A revised deterministic fixture source".into();
        let directory = runtime
            .workspace
            .scope_skills_root("owner", "default")
            .join("fixture-source");
        write_source_skill(&directory, "fixture-source", &changed);
        let updated = runtime
            .put_subscription(
                "owner",
                "default",
                &subscription.subscription_id,
                put_input(Some(before.revision)),
            )
            .await
            .unwrap();

        assert_ne!(updated.source_revision, before.source_revision);
        assert!(updated.cursor.is_none());
        assert!(updated.validators.is_empty());
        assert!(updated.seen_fingerprints.is_empty());
        assert!(updated.last_observed_identity.is_none());
    }

    #[tokio::test]
    async fn public_subscription_view_omits_runner_checkpoints_and_lease_state() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(
            vec![candidate("one", "https://example.com/one", "AI research")],
            true,
        ));
        let runtime = runtime_with_adapter(temp.path(), adapter, &[("owner", "default")]);
        let subscription = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();
        runtime
            .run_now("owner", "default", &subscription.subscription_id)
            .await
            .unwrap();
        let internal = runtime
            .list_subscriptions("owner", "default", None, None, None, 5)
            .await
            .unwrap()
            .items
            .remove(0);
        assert!(!internal.validators.is_empty());
        assert!(!internal.seen_fingerprints.is_empty());

        let public = serde_json::to_value(ObservationSubscriptionView::from(internal)).unwrap();

        assert_eq!(public["subscription_id"], subscription.subscription_id);
        for internal_field in [
            "cursor",
            "validators",
            "seen_fingerprints",
            "last_observed_identity",
            "lease_owner",
            "lease_expires_at_ms",
            "action_bindings",
        ] {
            assert!(
                public.get(internal_field).is_none(),
                "leaked {internal_field}"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_run_now_calls_acquire_only_one_lease() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(
            vec![candidate("one", "https://example.com/one", "AI research")],
            false,
        ));
        adapter.delay_ms.store(50, Ordering::SeqCst);
        let runtime =
            runtime_with_adapter(temp.path(), Arc::clone(&adapter), &[("owner", "default")]);
        let subscription = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();

        let (first, second) = tokio::join!(
            runtime.run_now("owner", "default", &subscription.subscription_id),
            runtime.run_now("owner", "default", &subscription.subscription_id)
        );
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.metrics().runs_started, 1);
    }

    #[tokio::test]
    async fn manual_runs_share_the_configured_global_concurrency_cap() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(Vec::new(), false));
        adapter.delay_ms.store(50, Ordering::SeqCst);
        let mut settings = ObservableSourceSettings::default();
        settings.max_concurrency = 1;
        let runtime = runtime_with_adapter_and_settings(
            temp.path(),
            Arc::clone(&adapter),
            &[("owner", "default"), ("guest", "default")],
            settings,
        );
        let owner = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();
        let guest = runtime
            .put_subscription("guest", "default", "auto", put_input(None))
            .await
            .unwrap();

        let (first, second) = tokio::join!(
            runtime.run_now("owner", "default", &owner.subscription_id),
            runtime.run_now("guest", "default", &guest.subscription_id)
        );

        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.metrics().runs_throttled, 1);
    }

    #[tokio::test]
    async fn failed_exact_action_backs_off_without_handoff_or_fallback() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(Vec::new(), false));
        adapter.fail.store(true, Ordering::SeqCst);
        let runtime =
            runtime_with_adapter(temp.path(), Arc::clone(&adapter), &[("owner", "default")]);
        let subscription = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();

        assert!(runtime
            .run_now("owner", "default", &subscription.subscription_id)
            .await
            .is_err());
        let failed = runtime
            .list_subscriptions("owner", "default", None, None, None, 5)
            .await
            .unwrap()
            .items
            .remove(0);
        assert_eq!(failed.consecutive_failures, 1);
        assert_eq!(
            failed.last_error_class.as_deref(),
            Some("acquisition_failed")
        );
        assert!(failed.next_run_at_ms > Utc::now().timestamp_millis());
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        assert!(!runtime.ingress_dir("owner", "default").exists());
        let observability = runtime
            .list_observability("owner", "default", None, 5)
            .await
            .unwrap();
        assert_eq!(observability.totals.runs, 1);
        assert_eq!(observability.totals.failed, 1);
        assert_eq!(observability.totals.degraded, 1);
        assert_eq!(
            observability.items[0]
                .last_run
                .as_ref()
                .and_then(|run| run.error_class.as_deref()),
            Some("acquisition_failed")
        );
    }

    #[tokio::test]
    async fn pause_during_fetch_cancels_commit_and_leaves_no_handoff() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(
            vec![candidate("one", "https://example.com/one", "AI research")],
            false,
        ));
        adapter.delay_ms.store(100, Ordering::SeqCst);
        let runtime =
            runtime_with_adapter(temp.path(), Arc::clone(&adapter), &[("owner", "default")]);
        let subscription = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();
        let run_runtime = Arc::clone(&runtime);
        let run_id = subscription.subscription_id.clone();
        let running =
            tokio::spawn(async move { run_runtime.run_now("owner", "default", &run_id).await });
        while adapter.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let leased = runtime
            .list_subscriptions("owner", "default", None, None, None, 5)
            .await
            .unwrap()
            .items
            .remove(0);
        let mut pause = put_input(Some(leased.revision));
        pause.enabled = false;
        runtime
            .put_subscription("owner", "default", &subscription.subscription_id, pause)
            .await
            .unwrap();

        assert!(matches!(
            running.await.unwrap(),
            Err(SubscriptionMutationError::Busy)
        ));
        assert!(!runtime.ingress_dir("owner", "default").exists());
        let paused = runtime
            .list_subscriptions("owner", "default", None, None, None, 5)
            .await
            .unwrap()
            .items
            .remove(0);
        assert!(!paused.enabled);
        assert!(paused.cursor.is_none());
        assert!(paused.validators.is_empty());
    }

    #[tokio::test]
    async fn manifest_policy_change_is_enforced_before_network_work() {
        let temp = tempdir().unwrap();
        let adapter = Arc::new(FakeRssAdapter::new(Vec::new(), false));
        let runtime =
            runtime_with_adapter(temp.path(), Arc::clone(&adapter), &[("owner", "default")]);
        let subscription = runtime
            .put_subscription("owner", "default", "auto", put_input(None))
            .await
            .unwrap();
        write_source_manifest(
            &runtime.workspace,
            "owner",
            "default",
            "fixture-source",
            "browser.headless.discover_handoff",
        );

        assert!(matches!(
            runtime
                .run_now("owner", "default", &subscription.subscription_id)
                .await,
            Err(SubscriptionMutationError::Unavailable)
        ));
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
    }
}
