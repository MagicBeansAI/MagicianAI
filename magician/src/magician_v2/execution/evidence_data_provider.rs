//! Scoped, read-only Apps binder for the outward-claims, work-evidence,
//! entity, and commitment registers.
//!
//! This provider is deliberately narrower than the first-party APIs. It owns
//! five bounded reads, accepts only executor-owned runtime scope, and has no
//! mutation path. Evidence and entity reads additionally require an explicit
//! agent id: those stores have agent and user-owned lanes, so silently unioning
//! every agent would turn one app grant into a workspace-wide data read.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use magicllm::LlmScope;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityPackDefinition, CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::agents::memory::AgentMemoryService;
use crate::magician_v2::agents::storage::validate_agent_identifier;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::{AudienceKind, AudienceRef};
use crate::magician_v2::commitments::{Commitment, CommitmentScope, CommitmentStatus, Commitments};
use crate::magician_v2::evidence::outward_assertions::OutwardScope;
use crate::magician_v2::evidence::transcript_ingestion::{
    TranscriptClaim, TranscriptClaimStatus, TranscriptIngestion,
};
use crate::magician_v2::evidence::{fold_was_cancelled, EvidenceRecord, EvidenceStatus};
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::resource_authority::scoped_authority::is_safe_scope_id;
use crate::magician_v2::strategy::plan::PlanStep;

pub const EVIDENCE_DATA_TOOL_NAME: &str = "evidence_data";

pub(crate) const APP_BOUND_EVIDENCE_DATA_INPUT_CEILING: u64 = 8 * 1024;
pub(crate) const APP_BOUND_EVIDENCE_DATA_RESULT_CEILING: u64 = 512 * 1024;

const DEFAULT_LIST_LIMIT: usize = 20;
const MAX_LIST_LIMIT: u64 = 50;
const MAX_ID_BYTES: usize = 255;
const MAX_TEXT_FILTER_BYTES: usize = 512;
const CLAIM_SCAN_BUDGET: usize = 4096;
const COMMITMENT_SCAN_BUDGET: usize = 4096;

const ACTIONS: &[&str] = &[
    "list_pending_claims",
    "read_claim",
    "list_evidence_records",
    "list_entities",
    "list_commitments",
];
const CLAIM_STATUSES: &[&str] = &["pending", "confirmed", "rejected", "all"];
const COMMITMENT_STATUSES: &[&str] =
    &["unconfirmed", "confirmed", "superseded", "withdrawn", "all"];

#[derive(Clone)]
pub struct EvidenceDataProvider {
    workspace_layout: ArtifactV2Workspace,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for EvidenceDataProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvidenceDataProvider")
            .field("workspace_layout", &self.workspace_layout)
            .field("pack_def", &self.pack_def.as_ref().map(|pack| &pack.name))
            .finish()
    }
}

impl EvidenceDataProvider {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            pack_def: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for EvidenceDataProvider {
    fn tool_name(&self) -> &str {
        EVIDENCE_DATA_TOOL_NAME
    }

    fn prove_app_tool_args(&self, parameters: &HashMap<String, Value>) -> bool {
        prove_app_evidence_data_args(parameters)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: EVIDENCE_DATA_TOOL_NAME.to_owned(),
            implementation: ImplementationType::Compiled {
                provider_name: EVIDENCE_DATA_TOOL_NAME.to_owned(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let params = match action {
            ExecutableAction::Pack {
                resolved_params, ..
            } => resolved_params.clone(),
            _ => {
                return Err(ExecutionError::Step(
                    "evidence_data: unexpected action type".to_owned(),
                ))
            },
        };
        let action_name = string_param(&params, "__action_name")
            .or_else(|| string_param(&params, "action"))
            .unwrap_or_else(|| "list_pending_claims".to_owned());
        let mut params = authorize_runtime_scope(params)?;
        params.insert(
            "__action_name".to_owned(),
            Value::String(action_name.clone()),
        );
        if !prove_app_evidence_data_args(&params) {
            return Err(ExecutionError::Step(
                "evidence_data arguments are outside the closed action schema".to_owned(),
            ));
        }
        let input_bytes = serde_json::to_vec(&params).map_err(|error| {
            ExecutionError::Step(format!(
                "evidence_data argument serialization failed: {error}"
            ))
        })?;
        if input_bytes.len() as u64 > APP_BOUND_EVIDENCE_DATA_INPUT_CEILING {
            return Err(ExecutionError::Step(format!(
                "evidence_data arguments exceeded the {} byte ceiling",
                APP_BOUND_EVIDENCE_DATA_INPUT_CEILING
            )));
        }
        let effective_timeout = timeout_secs.max(1);
        let value = timeout(
            Duration::from_secs(effective_timeout),
            execute_evidence_data_action(&self.workspace_layout, &action_name, &params),
        )
        .await
        .map_err(|_| {
            ExecutionError::Step(format!(
                "evidence_data action `{action_name}` timed out after {effective_timeout}s"
            ))
        })??;
        let rendered = serde_json::to_string_pretty(&value).map_err(|error| {
            ExecutionError::Step(format!(
                "evidence_data result serialization failed: {error}"
            ))
        })?;
        if rendered.len() as u64 > APP_BOUND_EVIDENCE_DATA_RESULT_CEILING {
            return Err(ExecutionError::Step(format!(
                "evidence_data result exceeded the {} byte ceiling",
                APP_BOUND_EVIDENCE_DATA_RESULT_CEILING
            )));
        }
        Ok(ActionResult::text(rendered))
    }

    fn default_timeout_secs(&self) -> u64 {
        self.pack_def
            .as_ref()
            .and_then(|pack| pack.execution.as_ref())
            .and_then(|execution| execution.default_timeout_secs)
            .unwrap_or(30)
    }
}

fn prove_app_evidence_data_args(parameters: &HashMap<String, Value>) -> bool {
    let Some(operation) = parameters.get("__action_name").and_then(Value::as_str) else {
        return false;
    };
    if !ACTIONS.contains(&operation) {
        return false;
    }

    for (key, value) in parameters {
        match key.as_str() {
            "__action_name" => {},
            "operation" | "action" | "method" => {
                let agrees = value.as_str().is_some_and(|alias| {
                    crate::magician_v2::apps::app_tool_bind::normalize_app_action_selector(
                        EVIDENCE_DATA_TOOL_NAME,
                        alias,
                    )
                    .as_deref()
                        == Some(operation)
                });
                if !agrees {
                    return false;
                }
            },
            "principal" | "workspace" => {
                if !bounded_nonblank_string(value, MAX_ID_BYTES) {
                    return false;
                }
            },
            "status" if matches!(operation, "list_pending_claims" | "list_commitments") => {
                let admitted = if operation == "list_pending_claims" {
                    CLAIM_STATUSES
                } else {
                    COMMITMENT_STATUSES
                };
                if value
                    .as_str()
                    .is_none_or(|status| !admitted.contains(&status))
                {
                    return false;
                }
            },
            "audience_kind" if matches!(operation, "list_pending_claims" | "list_commitments") => {
                if value.as_str().and_then(AudienceKind::parse).is_none() {
                    return false;
                }
            },
            "audience_id" if matches!(operation, "list_pending_claims" | "list_commitments") => {
                if !bounded_safe_id(value) {
                    return false;
                }
            },
            "text" if operation == "list_pending_claims" => {
                if !bounded_nonblank_string(value, MAX_TEXT_FILTER_BYTES) {
                    return false;
                }
            },
            "limit"
                if matches!(
                    operation,
                    "list_pending_claims"
                        | "list_evidence_records"
                        | "list_entities"
                        | "list_commitments"
                ) =>
            {
                if value
                    .as_u64()
                    .is_none_or(|limit| !(1..=MAX_LIST_LIMIT).contains(&limit))
                {
                    return false;
                }
            },
            "claim_id" if operation == "read_claim" => {
                if !bounded_safe_id(value) {
                    return false;
                }
            },
            "after_claim_id" if operation == "list_pending_claims" => {
                if !bounded_safe_id(value) {
                    return false;
                }
            },
            "agent_id" if matches!(operation, "list_evidence_records" | "list_entities") => {
                if value
                    .as_str()
                    .is_none_or(|agent_id| validate_agent_identifier(agent_id).is_err())
                {
                    return false;
                }
            },
            "after_record_id" if operation == "list_evidence_records" => {
                if !bounded_safe_id(value) {
                    return false;
                }
            },
            "after_entity_key" if operation == "list_entities" => {
                if !bounded_canonical_string(value, MAX_ID_BYTES) {
                    return false;
                }
            },
            "after_term_id" if operation == "list_commitments" => {
                if !bounded_safe_id(value) {
                    return false;
                }
            },
            hidden if hidden.starts_with("__") => {},
            _ => return false,
        }
    }

    let audience_kind = parameters.contains_key("audience_kind");
    let audience_id = parameters.contains_key("audience_id");
    if matches!(operation, "list_pending_claims" | "list_commitments")
        && audience_kind != audience_id
    {
        return false;
    }
    if operation == "list_commitments" && !audience_kind {
        return false;
    }
    match operation {
        "read_claim" => parameters.contains_key("claim_id"),
        "list_evidence_records" | "list_entities" => parameters.contains_key("agent_id"),
        _ => true,
    }
}

fn bounded_nonblank_string(value: &Value, max_bytes: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.trim().is_empty() && text.len() <= max_bytes)
}

fn bounded_canonical_string(value: &Value, max_bytes: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.is_empty() && text.trim() == text && text.len() <= max_bytes)
}

fn bounded_safe_id(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|id| id.trim() == id && id.len() <= MAX_ID_BYTES && is_safe_scope_id(id))
}

async fn execute_evidence_data_action(
    workspace_layout: &ArtifactV2Workspace,
    action: &str,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    match action {
        // These three stores expose synchronous durable reads. Keep them off
        // the async runtime worker so the outer timeout remains effective and
        // one large historical register cannot stall unrelated app dispatch.
        "list_pending_claims" => {
            let workspace_layout = workspace_layout.clone();
            let params = params.clone();
            read_on_blocking_worker("claim read", move |cancellation| {
                list_pending_claims(&workspace_layout, &params, cancellation)
            })
            .await
        },
        "read_claim" => {
            let workspace_layout = workspace_layout.clone();
            let params = params.clone();
            read_on_blocking_worker("exact claim read", move |cancellation| {
                read_claim(&workspace_layout, &params, cancellation)
            })
            .await
        },
        "list_evidence_records" => list_evidence_records(workspace_layout, params).await,
        "list_entities" => list_entities(workspace_layout, params).await,
        "list_commitments" => {
            let workspace_layout = workspace_layout.clone();
            let params = params.clone();
            read_on_blocking_worker("commitment read", move |cancellation| {
                list_commitments(&workspace_layout, &params, cancellation)
            })
            .await
        },
        other => Err(ExecutionError::Step(format!(
            "evidence_data: unknown action `{other}`"
        ))),
    }
}

/// Run one synchronous register read on a blocking worker, and abandon its fold
/// if this call is dropped before the answer arrives.
///
/// `spawn_blocking` is what keeps a large register off the async workers, but
/// dropping the join handle when the outer timeout fires does not stop the
/// thread: it keeps folding a log nobody will read and holds the whole thing in
/// memory while it does. The drop guard lives in *this* future, so the same drop
/// that abandons the handle cancels the token the fold is watching, and the
/// worker stops within one check interval
/// (`magician_v2::evidence::store_cursor`).
async fn read_on_blocking_worker<Read>(what: &str, read: Read) -> Result<Value, ExecutionError>
where
    Read: FnOnce(&CancellationToken) -> Result<Value, ExecutionError> + Send + 'static,
{
    let cancellation = CancellationToken::new();
    let watched_by_the_fold = cancellation.clone();
    let _abandon_on_drop = cancellation.drop_guard();
    tokio::task::spawn_blocking(move || read(&watched_by_the_fold))
        .await
        .map_err(|error| {
            ExecutionError::Step(format!("evidence_data {what} worker failed: {error}"))
        })?
}

fn list_pending_claims(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    cancellation: &CancellationToken,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let store_scope = OutwardScope::new(&scope.principal, &scope.workspace);
    // The queue is paged only after the whole log is folded, so a caller that
    // stopped waiting has to be able to stop the fold too.
    let mut claims = TranscriptIngestion::new(workspace_layout.clone())
        .claims_until_cancelled(&store_scope, cancellation)
        .map_err(|error| store_error("reading the transcript claim register", error))?;
    claims.sort_by(claim_order);

    let cursor = cursor_index(&claims, params, "after_claim_id", |claim| {
        claim.claim_id.as_str()
    })?;
    let remaining = &claims[cursor..];
    let scan_truncated = remaining.len() > CLAIM_SCAN_BUDGET;
    let scanned = &remaining[..remaining.len().min(CLAIM_SCAN_BUDGET)];
    let wanted_status = string_param(params, "status").unwrap_or_else(|| "pending".to_owned());
    let audience = optional_audience(params)?;
    let text = string_param(params, "text").map(|text| text.to_lowercase());
    let mut matches = scanned
        .iter()
        .filter(|claim| claim_status_matches(claim.status, &wanted_status))
        .filter(|claim| {
            audience
                .as_ref()
                .is_none_or(|wanted| claim.audience_ref.as_ref() == Some(wanted))
        })
        .filter(|claim| {
            text.as_ref()
                .is_none_or(|needle| claim.stated_text.to_lowercase().contains(needle))
        })
        .collect::<Vec<_>>();
    let limit = bounded_limit(params);
    let page_has_more = matches.len() > limit;
    matches.truncate(limit);
    let next_cursor = if page_has_more {
        matches.last().map(|claim| claim.claim_id.clone())
    } else if scan_truncated {
        scanned.last().map(|claim| claim.claim_id.clone())
    } else {
        None
    };
    let claims = matches
        .into_iter()
        .map(VisibleClaimProjection::from)
        .collect::<Vec<_>>();
    Ok(json!({
        "scope": scope.as_json(),
        "status": wanted_status,
        "audience": audience,
        "count": claims.len(),
        "scanned": scanned.len(),
        "scan_truncated": scan_truncated,
        "next_cursor": next_cursor,
        "claims": claims,
    }))
}

/// The list action is a queue projection, not a raw claim-register export.
/// In particular, provenance, decision notes, assertion ids and runtime scope
/// do not cross the app tool boundary merely because the package needs the
/// reviewed words and optimistic-concurrency revision.
#[derive(Debug, Serialize)]
struct VisibleClaimProjection<'a> {
    claim_id: &'a str,
    status: TranscriptClaimStatus,
    stated_text: &'a str,
    speaker: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    audience_ref: Option<&'a AudienceRef>,
    extracted_at: &'a chrono::DateTime<chrono::Utc>,
    revision: u64,
}

impl<'a> From<&'a TranscriptClaim> for VisibleClaimProjection<'a> {
    fn from(claim: &'a TranscriptClaim) -> Self {
        Self {
            claim_id: &claim.claim_id,
            status: claim.status,
            stated_text: &claim.stated_text,
            speaker: &claim.speaker,
            audience_ref: claim.audience_ref.as_ref(),
            extracted_at: &claim.extracted_at,
            revision: claim.revision,
        }
    }
}

fn read_claim(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    cancellation: &CancellationToken,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let claim_id = required_string(params, "claim_id")?;
    let store_scope = OutwardScope::new(&scope.principal, &scope.workspace);
    // `TranscriptIngestion::claim` is this same whole-log fold with the find
    // done for us, but uncancellably. An exact read is no cheaper than the list
    // it is cut from, so it is abandoned on the same signal.
    let claim = TranscriptIngestion::new(workspace_layout.clone())
        .claims_until_cancelled(&store_scope, cancellation)
        .map_err(|error| store_error("reading the transcript claim register", error))?
        .into_iter()
        .find(|claim| claim.claim_id == claim_id)
        .ok_or_else(|| {
            ExecutionError::Step(format!("evidence_data: claim not found: {claim_id}"))
        })?;
    // The claim carries the exact utterance and its transcript/segment keys.
    // Returning that bounded context avoids loading the entire immutable
    // transcript payload merely to repeat the one utterance under review.
    let claim_projection = VisibleClaimDetailProjection::from(&claim);
    let context = json!({
        "transcript_key": &claim.transcript_key,
        "segment_key": &claim.segment_key,
        "speaker": &claim.speaker,
        "stated_text": &claim.stated_text,
    });
    Ok(json!({
        "scope": scope.as_json(),
        "claim": claim_projection,
        "utterance_context": context,
    }))
}

/// Exact detail still returns only fields the detail workflow declares. The
/// utterance itself is carried separately in `utterance_context`; returning the
/// full register row here would also disclose evidence refs, assertion-use ids
/// and historical decision notes to a package that never consumes them.
#[derive(Debug, Serialize)]
struct VisibleClaimDetailProjection<'a> {
    claim_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    audience_ref: Option<&'a AudienceRef>,
    extracted_by: &'a str,
    extracted_at: &'a chrono::DateTime<chrono::Utc>,
    revision: u64,
}

impl<'a> From<&'a TranscriptClaim> for VisibleClaimDetailProjection<'a> {
    fn from(claim: &'a TranscriptClaim) -> Self {
        Self {
            claim_id: &claim.claim_id,
            audience_ref: claim.audience_ref.as_ref(),
            extracted_by: &claim.extracted_by,
            extracted_at: &claim.extracted_at,
            revision: claim.revision,
        }
    }
}

async fn list_evidence_records(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let agent_id = required_string(params, "agent_id")?;
    validate_agent_identifier(&agent_id).map_err(|error| {
        ExecutionError::Step(format!("evidence_data: invalid agent_id: {error}"))
    })?;
    let memory = AgentMemoryService::with_scoped_memory_scope_in_workspace(
        workspace_layout.clone(),
        scope.principal.as_str(),
        scope.workspace.as_str(),
    );
    let mut records = memory
        .load_scoped_evidence(&agent_id)
        .await
        .map_err(|error| {
            ExecutionError::Step(format!("evidence_data evidence read failed: {error}"))
        })?;
    // load_scoped_evidence is the authoritative affirmatively-sensitive
    // default-suppression owner. Do not replace it with load_native_evidence.
    prepare_visible_evidence(&mut records)?;
    let cursor = cursor_index(&records, params, "after_record_id", |record| {
        record.evidence_id.as_str()
    })?;
    let page = bounded_page(&records[cursor..], bounded_limit(params));
    let records = page
        .items
        .into_iter()
        .map(VisibleEvidenceProjection::from)
        .collect::<Vec<_>>();
    Ok(json!({
        "scope": scope.as_json(),
        "agent_id": agent_id,
        "count": records.len(),
        "scan_truncated": false,
        "next_cursor": page.next_cursor,
        "records": records,
    }))
}

/// Correction context is intentionally narrower than `EvidenceRecord`.
/// Source refs, observed actions, artifacts, facets, entity/person keys,
/// confidence, sensitivity and the extensible metadata bag can all carry
/// private provenance and are not used by the claims-review projection.
#[derive(Debug, Serialize)]
struct VisibleEvidenceProjection<'a> {
    evidence_id: &'a str,
    status: EvidenceStatus,
    summary: &'a str,
    first_seen_at: &'a str,
}

impl<'a> From<&'a EvidenceRecord> for VisibleEvidenceProjection<'a> {
    fn from(record: &'a EvidenceRecord) -> Self {
        Self {
            evidence_id: &record.evidence_id,
            status: record.status,
            summary: &record.summary,
            first_seen_at: &record.first_seen_at,
        }
    }
}

fn prepare_visible_evidence(records: &mut Vec<EvidenceRecord>) -> Result<(), ExecutionError> {
    // A Deleted row is a durable anti-resurrection tombstone, not correction
    // context. Its domain contract says it is excluded everywhere; exposing its
    // old summary through this package would undo the user's deletion at read
    // time. Suppressed rows remain visible so the correction UI can inspect and
    // potentially un-suppress them.
    let mut seen = HashSet::with_capacity(records.len());
    if let Some(duplicate) = records
        .iter()
        .find(|record| !seen.insert(record.evidence_id.as_str()))
    {
        return Err(ExecutionError::Step(format!(
            "evidence_data: duplicate evidence id `{}` makes the scoped cursor ambiguous",
            duplicate.evidence_id
        )));
    }
    records.retain(|record| record.status != EvidenceStatus::Deleted);
    records.sort_by(evidence_order);
    Ok(())
}

async fn list_entities(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let agent_id = required_string(params, "agent_id")?;
    validate_agent_identifier(&agent_id).map_err(|error| {
        ExecutionError::Step(format!("evidence_data: invalid agent_id: {error}"))
    })?;
    let memory = AgentMemoryService::with_scoped_memory_scope_in_workspace(
        workspace_layout.clone(),
        scope.principal.as_str(),
        scope.workspace.as_str(),
    );
    // Stored entity anchors merge aliases, facets, provenance, and timestamps
    // from every contributing evidence record, but sensitivity is owned by the
    // evidence record rather than the anchor. Filtering a merged anchor by key
    // is therefore insufficient: one visible contribution could admit fields
    // accumulated from another sensitive contribution. Build this minimal
    // projection exclusively from the authoritative sensitivity-suppressed
    // evidence view instead.
    let mut visible_evidence = memory
        .load_scoped_evidence(&agent_id)
        .await
        .map_err(|error| {
            ExecutionError::Step(format!(
                "evidence_data entity sensitivity proof failed: {error}"
            ))
        })?;
    prepare_visible_evidence(&mut visible_evidence)?;
    let mut entities = visible_entity_projections(&visible_evidence);
    entities.sort_by(entity_order);
    let cursor = cursor_index(&entities, params, "after_entity_key", |entity| {
        entity.entity_key.as_str()
    })?;
    let page = bounded_page(&entities[cursor..], bounded_limit(params));
    Ok(json!({
        "scope": scope.as_json(),
        "agent_id": agent_id,
        "count": page.items.len(),
        "scan_truncated": false,
        "next_cursor": page.next_cursor,
        "entities": page.items,
    }))
}

#[derive(Debug, Clone, Serialize)]
struct VisibleEntityProjection {
    entity_key: String,
    entity_type: String,
    canonical_name: String,
    first_seen_at: String,
    last_seen_at: String,
}

fn visible_entity_projections(records: &[EvidenceRecord]) -> Vec<VisibleEntityProjection> {
    let mut by_key = BTreeMap::<String, VisibleEntityProjection>::new();
    for record in records {
        for raw_key in record.entity_keys.iter().chain(record.people_keys.iter()) {
            let entity_key = raw_key.trim().to_lowercase();
            if entity_key.is_empty() {
                continue;
            }
            let (entity_type, slug) = match entity_key.split_once(':') {
                Some((kind, slug)) if !slug.trim().is_empty() => {
                    (kind.trim().to_owned(), slug.trim().to_owned())
                },
                _ => ("other".to_owned(), entity_key.clone()),
            };
            let canonical_name = visible_entity_name(&entity_type, &slug);
            by_key
                .entry(entity_key.clone())
                .and_modify(|entity| {
                    if record.first_seen_at < entity.first_seen_at {
                        entity.first_seen_at.clone_from(&record.first_seen_at);
                    }
                    if record.last_seen_at > entity.last_seen_at {
                        entity.last_seen_at.clone_from(&record.last_seen_at);
                    }
                })
                .or_insert_with(|| VisibleEntityProjection {
                    entity_key,
                    entity_type,
                    canonical_name,
                    first_seen_at: record.first_seen_at.clone(),
                    last_seen_at: record.last_seen_at.clone(),
                });
        }
    }
    by_key.into_values().collect()
}

fn visible_entity_name(entity_type: &str, slug: &str) -> String {
    match entity_type {
        "pr" | "ticket" => format!("{} {}", entity_type.to_uppercase(), slug),
        _ => slug
            .split(['-', '_', ' '])
            .filter(|word| !word.is_empty())
            .map(|word| {
                let mut characters = word.chars();
                match characters.next() {
                    Some(first) => first.to_uppercase().chain(characters).collect::<String>(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn list_commitments(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    cancellation: &CancellationToken,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let audience = optional_audience(params)?.ok_or_else(|| {
        ExecutionError::Step(
            "evidence_data: list_commitments requires audience_kind and audience_id".to_owned(),
        )
    })?;
    let commitment_scope = CommitmentScope::new(&scope.principal, &scope.workspace);
    let store = Commitments::new(workspace_layout.clone());
    // One audience shard is still folded whole before this page is cut, so the
    // fold has to end when the caller does.
    let mut commitments = store
        .for_audience_until_cancelled(&commitment_scope, &audience, cancellation)
        .map_err(|error| store_error("reading the commitment register", error))?;
    commitments.sort_by(commitment_order);

    let cursor = cursor_index(&commitments, params, "after_term_id", |commitment| {
        commitment.commitment_id.as_str()
    })?;
    let remaining = &commitments[cursor..];
    let scan_truncated = remaining.len() > COMMITMENT_SCAN_BUDGET;
    let scanned = &remaining[..remaining.len().min(COMMITMENT_SCAN_BUDGET)];
    let wanted_status = string_param(params, "status").unwrap_or_else(|| "all".to_owned());
    let mut matches = scanned
        .iter()
        .filter(|commitment| commitment_status_matches(commitment.status, &wanted_status))
        .collect::<Vec<_>>();
    let limit = bounded_limit(params);
    let page_has_more = matches.len() > limit;
    matches.truncate(limit);
    let next_cursor = if page_has_more {
        matches.last().map(|held| held.commitment_id.clone())
    } else if scan_truncated {
        scanned.last().map(|held| held.commitment_id.clone())
    } else {
        None
    };
    let restatable_outward = matches
        .iter()
        .filter(|held| held.may_be_restated_outward())
        .count();
    let commitments = matches
        .into_iter()
        .map(VisibleCommitmentProjection::from)
        .collect::<Vec<_>>();
    Ok(json!({
        "scope": scope.as_json(),
        "audience": audience,
        "status": wanted_status,
        "count": commitments.len(),
        "restatable_outward": restatable_outward,
        "scanned": scanned.len(),
        "scan_truncated": scan_truncated,
        "next_cursor": next_cursor,
        "commitments": commitments,
    }))
}

/// One relationship-bound review projection. The source reference and
/// supersession internals are host provenance, not inputs to this package's
/// confirmation workflow.
#[derive(Debug, Serialize)]
struct VisibleCommitmentProjection<'a> {
    commitment_id: &'a str,
    audience: &'a AudienceRef,
    direction: crate::magician_v2::commitments::CommitmentDirection,
    terms: &'a str,
    status: CommitmentStatus,
    recorded_at: &'a chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confirmed_by: Option<&'a str>,
    revision: u64,
}

impl<'a> From<&'a Commitment> for VisibleCommitmentProjection<'a> {
    fn from(commitment: &'a Commitment) -> Self {
        Self {
            commitment_id: &commitment.commitment_id,
            audience: &commitment.audience,
            direction: commitment.direction,
            terms: &commitment.terms,
            status: commitment.status,
            recorded_at: &commitment.recorded_at,
            confirmed_by: commitment.confirmed_by.as_deref(),
            revision: commitment.revision,
        }
    }
}

fn optional_audience(
    params: &HashMap<String, Value>,
) -> Result<Option<AudienceRef>, ExecutionError> {
    match (
        string_param(params, "audience_kind"),
        string_param(params, "audience_id"),
    ) {
        (None, None) => Ok(None),
        (Some(kind), Some(id)) => {
            let kind = AudienceKind::parse(&kind).ok_or_else(|| {
                ExecutionError::Step(format!("evidence_data: unknown audience kind `{kind}`"))
            })?;
            if id.len() > MAX_ID_BYTES || !is_safe_scope_id(&id) {
                return Err(ExecutionError::Step(
                    "evidence_data: audience_id failed the safe-identifier check".to_owned(),
                ));
            }
            Ok(Some(AudienceRef::new(kind, id)))
        },
        _ => Err(ExecutionError::Step(
            "evidence_data: audience_kind and audience_id must be supplied together".to_owned(),
        )),
    }
}

fn claim_status_matches(status: TranscriptClaimStatus, wanted: &str) -> bool {
    wanted == "all" || status.as_str() == wanted
}

fn commitment_status_matches(status: CommitmentStatus, wanted: &str) -> bool {
    wanted == "all" || status.as_str() == wanted
}

fn claim_order(left: &TranscriptClaim, right: &TranscriptClaim) -> Ordering {
    left.extracted_at
        .cmp(&right.extracted_at)
        .then_with(|| left.claim_id.cmp(&right.claim_id))
}

fn evidence_order(left: &EvidenceRecord, right: &EvidenceRecord) -> Ordering {
    left.first_seen_at
        .cmp(&right.first_seen_at)
        .then_with(|| left.evidence_id.cmp(&right.evidence_id))
}

fn entity_order(left: &VisibleEntityProjection, right: &VisibleEntityProjection) -> Ordering {
    left.first_seen_at
        .cmp(&right.first_seen_at)
        .then_with(|| left.entity_key.cmp(&right.entity_key))
}

fn commitment_order(left: &Commitment, right: &Commitment) -> Ordering {
    left.recorded_at
        .cmp(&right.recorded_at)
        .then_with(|| left.commitment_id.cmp(&right.commitment_id))
}

struct Page<'a, T> {
    items: Vec<&'a T>,
    next_cursor: Option<String>,
}

fn bounded_page<'a, T: CursorId>(values: &'a [T], limit: usize) -> Page<'a, T> {
    let has_more = values.len() > limit;
    let items = values.iter().take(limit).collect::<Vec<_>>();
    let next_cursor = has_more
        .then(|| items.last().map(|value| value.cursor_id().to_owned()))
        .flatten();
    Page { items, next_cursor }
}

trait CursorId {
    fn cursor_id(&self) -> &str;
}

impl CursorId for EvidenceRecord {
    fn cursor_id(&self) -> &str {
        &self.evidence_id
    }
}

impl CursorId for VisibleEntityProjection {
    fn cursor_id(&self) -> &str {
        &self.entity_key
    }
}

fn cursor_index<T>(
    values: &[T],
    params: &HashMap<String, Value>,
    key: &str,
    id: impl Fn(&T) -> &str,
) -> Result<usize, ExecutionError> {
    let Some(cursor) = string_param(params, key) else {
        return Ok(0);
    };
    values
        .iter()
        .position(|value| id(value) == cursor)
        .map(|index| index + 1)
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "evidence_data: `{key}` does not name a record in the current scoped ordering"
            ))
        })
}

#[derive(Debug, Clone)]
struct Scope {
    principal: String,
    workspace: String,
}

impl Scope {
    fn as_json(&self) -> Value {
        json!({"principal": self.principal, "workspace": self.workspace})
    }
}

fn scope_from_params(params: &HashMap<String, Value>) -> Result<Scope, ExecutionError> {
    Ok(Scope {
        principal: required_runtime_scope_value(params, "__principal")?,
        workspace: required_runtime_scope_value(params, "__workspace")?,
    })
}

fn authorize_runtime_scope(
    mut params: HashMap<String, Value>,
) -> Result<HashMap<String, Value>, ExecutionError> {
    let principal = required_runtime_scope_value(&params, "__principal")?;
    let workspace = required_runtime_scope_value(&params, "__workspace")?;
    if !LlmScope::new(&principal, &workspace).is_valid() {
        return Err(ExecutionError::Step(
            "evidence_data runtime scope contains an unsafe principal or workspace component"
                .to_owned(),
        ));
    }
    for (public_key, trusted_value) in [
        ("principal", principal.as_str()),
        ("workspace", workspace.as_str()),
    ] {
        if let Some(value) = params.get(public_key) {
            let Value::String(value) = value else {
                return Err(ExecutionError::Step(format!(
                    "evidence_data: `{public_key}` is an optional scope assertion and must be a string"
                )));
            };
            if !value.is_empty() && value != trusted_value {
                return Err(ExecutionError::Step(format!(
                    "evidence_data: model-supplied `{public_key}` does not match the runtime-authorized scope"
                )));
            }
        }
    }
    params.insert("principal".to_owned(), Value::String(principal));
    params.insert("workspace".to_owned(), Value::String(workspace));
    Ok(params)
}

fn required_runtime_scope_value(
    params: &HashMap<String, Value>,
    key: &str,
) -> Result<String, ExecutionError> {
    let value = params.get(key).and_then(Value::as_str).ok_or_else(|| {
        ExecutionError::Step(format!(
            "evidence_data requires runtime-owned scope `{key}`; unscoped execution is denied"
        ))
    })?;
    if value.is_empty() || value.trim() != value {
        return Err(ExecutionError::Step(format!(
            "evidence_data runtime-owned scope `{key}` must be a nonblank canonical component"
        )));
    }
    Ok(value.to_owned())
}

fn bounded_limit(params: &HashMap<String, Value>) -> usize {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_LIST_LIMIT as u64)
        .clamp(1, MAX_LIST_LIMIT) as usize
}

fn string_param(params: &HashMap<String, Value>, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn required_string(params: &HashMap<String, Value>, key: &str) -> Result<String, ExecutionError> {
    string_param(params, key).ok_or_else(|| {
        ExecutionError::Step(format!("evidence_data: `{key}` must be a non-empty string"))
    })
}

fn store_error(context: &str, error: anyhow::Error) -> ExecutionError {
    // A caller that stopped waiting is not a register fault, and the two read
    // identically unless they are named apart. Only one of them is worth an
    // operator opening the log file.
    if fold_was_cancelled(&error) {
        return ExecutionError::Step(format!("evidence_data {context} was abandoned: {error}"));
    }
    ExecutionError::Step(format!("evidence_data {context} failed: {error:#}"))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn params(action: &str) -> HashMap<String, Value> {
        HashMap::from([("__action_name".to_owned(), Value::String(action.to_owned()))])
    }

    /// Runtime-owned scope, the way `authorize_runtime_scope` leaves it before
    /// a read runs.
    fn scoped(action: &str) -> HashMap<String, Value> {
        let mut scoped = params(action);
        scoped.insert("__principal".to_owned(), json!("owner"));
        scoped.insert("__workspace".to_owned(), json!("main"));
        scoped
    }

    fn commitment_params() -> HashMap<String, Value> {
        let mut commitments = scoped("list_commitments");
        commitments.insert("audience_kind".to_owned(), json!("engagement"));
        commitments.insert("audience_id".to_owned(), json!("acme"));
        commitments
    }

    fn claim_params() -> HashMap<String, Value> {
        let mut claim = scoped("read_claim");
        claim.insert("claim_id".to_owned(), json!("claim-1"));
        claim
    }

    #[test]
    fn app_argument_proof_is_closed_and_agent_reads_are_explicit() {
        let mut claims = params("list_pending_claims");
        claims.insert("limit".to_owned(), json!(25));
        claims.insert("text".to_owned(), json!("launch"));
        assert!(prove_app_evidence_data_args(&claims));
        claims.insert("unknown".to_owned(), json!(true));
        assert!(!prove_app_evidence_data_args(&claims));

        for action in ["list_evidence_records", "list_entities"] {
            let missing = params(action);
            assert!(!prove_app_evidence_data_args(&missing));
            let mut scoped = params(action);
            scoped.insert("agent_id".to_owned(), json!("review-agent"));
            assert!(prove_app_evidence_data_args(&scoped));
        }
    }

    #[test]
    fn audience_filters_are_pairwise_and_closed() {
        for action in ["list_pending_claims", "list_commitments"] {
            if action == "list_commitments" {
                assert!(!prove_app_evidence_data_args(&params(action)));
            }
            let mut kind_only = params(action);
            kind_only.insert("audience_kind".to_owned(), json!("engagement"));
            assert!(!prove_app_evidence_data_args(&kind_only));

            let mut pair = kind_only;
            pair.insert("audience_id".to_owned(), json!("customer-1"));
            assert!(prove_app_evidence_data_args(&pair));
            pair.insert("audience_kind".to_owned(), json!("public"));
            assert!(!prove_app_evidence_data_args(&pair));
        }
    }

    #[test]
    fn routing_aliases_must_agree_and_values_are_bounded() {
        let mut read = params("read_claim");
        read.insert("claim_id".to_owned(), json!("claim-1"));
        read.insert("action".to_owned(), json!("evidence_data__read_claim"));
        assert!(prove_app_evidence_data_args(&read));
        read.insert("action".to_owned(), json!("list_commitments"));
        assert!(!prove_app_evidence_data_args(&read));

        let mut list = params("list_pending_claims");
        list.insert("limit".to_owned(), json!(MAX_LIST_LIMIT + 1));
        assert!(!prove_app_evidence_data_args(&list));
        list.insert("limit".to_owned(), json!(1));
        list.insert(
            "text".to_owned(),
            json!("x".repeat(MAX_TEXT_FILTER_BYTES + 1)),
        );
        assert!(!prove_app_evidence_data_args(&list));
    }

    #[test]
    fn runtime_scope_is_required_and_public_scope_cannot_switch_it() {
        let unscoped = params("list_pending_claims");
        assert!(authorize_runtime_scope(unscoped).is_err());

        let mut scoped = params("list_pending_claims");
        scoped.insert("__principal".to_owned(), json!("owner"));
        scoped.insert("__workspace".to_owned(), json!("main"));
        scoped.insert("workspace".to_owned(), json!("other"));
        assert!(authorize_runtime_scope(scoped).is_err());
    }

    #[test]
    fn entity_projection_contains_only_visible_evidence_derived_fields() {
        let record: EvidenceRecord = serde_json::from_value(json!({
            "evidence_id": "evd:visible:1",
            "summary": "Visible project work",
            "evidence_kind": "work",
            "entity_keys": ["project:alpha"],
            "people_keys": ["person:casey"],
            "source_refs": ["source:visible"],
            "facets": [{"label": "confidential-project", "confidence": 0.9, "assigned_by": "llm"}],
            "importance": 0.7,
            "confidence": 0.8,
            "sensitivity": "normal",
            "first_seen_at": "2026-09-01T10:00:00Z",
            "last_seen_at": "2026-09-01T11:00:00Z"
        }))
        .expect("evidence fixture");

        let projections = visible_entity_projections(&[record]);
        assert_eq!(projections.len(), 2);
        for value in projections
            .iter()
            .map(|projection| serde_json::to_value(projection).expect("projection json"))
        {
            let object = value.as_object().expect("projection object");
            assert_eq!(object.len(), 5);
            for private_field in [
                "aliases",
                "source_refs",
                "facets",
                "confidence",
                "status",
                "merged_into",
                "user_curated",
                "producer",
            ] {
                assert!(!object.contains_key(private_field));
            }
        }
    }

    #[test]
    fn evidence_projection_omits_private_provenance_and_deleted_rows() {
        let visible: EvidenceRecord = serde_json::from_value(json!({
            "evidence_id": "evd:visible:1",
            "summary": "Visible project work",
            "evidence_kind": "work",
            "observed_actions": ["opened private repository"],
            "entity_keys": ["project:alpha"],
            "people_keys": ["person:casey"],
            "artifact_refs": ["artifact:private"],
            "source_refs": ["file:/private/source"],
            "facets": [{"label": "internal", "confidence": 0.9, "assigned_by": "llm"}],
            "importance": 0.7,
            "confidence": 0.8,
            "sensitivity": "normal",
            "first_seen_at": "2026-09-01T10:00:00Z",
            "last_seen_at": "2026-09-01T11:00:00Z",
            "metadata": {"private_note": "not package context"}
        }))
        .expect("visible evidence fixture");
        let mut deleted = visible.clone();
        deleted.evidence_id = "evd:deleted:1".to_owned();
        deleted.status = EvidenceStatus::Deleted;

        let mut records = vec![deleted, visible];
        prepare_visible_evidence(&mut records).expect("visible evidence preparation");
        assert_eq!(records.len(), 1);
        let value = serde_json::to_value(VisibleEvidenceProjection::from(&records[0]))
            .expect("evidence projection json");
        assert_eq!(
            value
                .as_object()
                .expect("projection object")
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(
                ["evidence_id", "first_seen_at", "status", "summary",]
            )
        );
        assert!(!value.to_string().contains("private"));
    }

    #[test]
    fn duplicate_evidence_ids_fail_closed_before_cursoring_or_tombstone_filtering() {
        let record: EvidenceRecord = serde_json::from_value(json!({
            "evidence_id": "evd:collision:1",
            "summary": "One lane",
            "evidence_kind": "work",
            "source_refs": ["source:one"],
            "importance": 0.7,
            "confidence": 0.8,
            "sensitivity": "normal",
            "first_seen_at": "2026-09-01T10:00:00Z",
            "last_seen_at": "2026-09-01T11:00:00Z"
        }))
        .expect("evidence fixture");
        let mut tombstone = record.clone();
        tombstone.status = EvidenceStatus::Deleted;
        let mut collision = vec![record, tombstone];
        assert!(prepare_visible_evidence(&mut collision).is_err());
    }

    // ── E4: the cancellable folds are wired, not merely defined ─────────────

    /// The gate's whole point: this provider hands the register folds a token
    /// they observe. The workspace is empty on purpose — both folds checkpoint
    /// *before* they read, so a refusal here can have come from nothing but the
    /// token, and a provider that quietly went back to `claims` /
    /// `for_audience` would hand back a page instead.
    #[test]
    fn a_cancelled_read_refuses_before_it_folds_either_register() {
        let tmp = TempDir::new().expect("workspace root");
        let workspace_layout = ArtifactV2Workspace::new(tmp.path());
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        for refusal in [
            list_pending_claims(
                &workspace_layout,
                &scoped("list_pending_claims"),
                &cancellation,
            ),
            read_claim(&workspace_layout, &claim_params(), &cancellation),
            list_commitments(&workspace_layout, &commitment_params(), &cancellation),
        ] {
            let error = refusal
                .expect_err("a cancelled fold refuses rather than returning rows")
                .to_string();
            assert!(
                error.contains("was abandoned"),
                "a cancelled read is the caller's own doing, not a register fault: {error}"
            );
        }
    }

    /// Cancellation is the only thing this wiring changed. An uncancelled read
    /// answers the ordinary way — an absent register is an empty page and an
    /// absent claim is not found — so the token cannot become a new way for a
    /// healthy read to fail.
    #[test]
    fn an_uncancelled_read_answers_the_ordinary_way() {
        let tmp = TempDir::new().expect("workspace root");
        let workspace_layout = ArtifactV2Workspace::new(tmp.path());
        let live = CancellationToken::new();

        let claims = list_pending_claims(&workspace_layout, &scoped("list_pending_claims"), &live)
            .expect("an empty claims log is an empty page");
        assert_eq!(claims["count"], json!(0));
        assert_eq!(claims["scan_truncated"], json!(false));

        let commitments = list_commitments(&workspace_layout, &commitment_params(), &live)
            .expect("an empty commitment shard is an empty page");
        assert_eq!(commitments["count"], json!(0));
        assert_eq!(commitments["scan_truncated"], json!(false));

        // "No such claim" is the package's answer; "the fold was stopped" is a
        // refusal. Routing the exact read through the cancellable fold must not
        // blur the two.
        let missing = read_claim(&workspace_layout, &claim_params(), &live)
            .expect_err("an absent claim is not found")
            .to_string();
        assert!(missing.contains("claim not found"), "{missing}");
    }

    /// The outer timeout is what fires the token. Dropping the future that
    /// awaits the worker drops the guard, so a fold still running is told to
    /// stop instead of being left to finish an answer nobody will read — the
    /// exact leak `spawn_blocking` alone does not close.
    #[tokio::test]
    async fn abandoning_the_call_cancels_the_fold_it_started() {
        let (started, worker_token) = std::sync::mpsc::channel();
        let (release, wait_for_release) = std::sync::mpsc::channel::<()>();
        let read = move |cancellation: &CancellationToken| -> Result<Value, ExecutionError> {
            started
                .send(cancellation.clone())
                .expect("hand the token back");
            // Stay inside the fold until the assertion is done, so the guard
            // drops against a live read rather than a finished one.
            let _ = wait_for_release.recv();
            Ok(json!({}))
        };

        let abandoned = timeout(
            Duration::from_millis(50),
            read_on_blocking_worker("test read", read),
        )
        .await;
        assert!(abandoned.is_err(), "the outer timeout wins this race");

        let handed = worker_token.recv().expect("the worker started");
        assert!(
            handed.is_cancelled(),
            "an abandoned call has to cancel the fold it started, or the blocking \
             thread keeps folding a register nobody will read"
        );
        let _ = release.send(());
    }

    /// The door the three tests above cannot hold shut. They prove the leaf
    /// reads observe the token they are handed, and that the helper cancels one
    /// when its caller gives up — neither notices if the dispatcher goes back
    /// to spawning a read itself, which is the shape this gate reopened on. The
    /// drop guard lives in `read_on_blocking_worker` and nowhere else, so that
    /// is the only blocking spawn this file may hold and every synchronous
    /// register read has to arrive through it.
    #[test]
    fn source_oracle_keeps_every_register_read_behind_the_cancelling_worker() {
        let source = include_str!("evidence_data_provider.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("production half");

        assert_eq!(
            production.matches("tokio::task::spawn_blocking(").count(),
            1,
            "the one blocking spawn belongs to read_on_blocking_worker, whose drop guard is the \
             only thing that stops a fold the caller has stopped waiting for"
        );
        for routed_read in [
            "read_on_blocking_worker(\"claim read\"",
            "read_on_blocking_worker(\"exact claim read\"",
            "read_on_blocking_worker(\"commitment read\"",
        ] {
            assert!(
                production.contains(routed_read),
                "a synchronous register read left the cancelling worker: {routed_read}"
            );
        }
        for uncancellable_fold in [
            ".claims(&store_scope)",
            ".claim(&store_scope,",
            ".for_audience(&commitment_scope,",
        ] {
            assert!(
                !production.contains(uncancellable_fold),
                "an uncancellable whole-log fold is back on a blocking worker: \
                 {uncancellable_fold}"
            );
        }
    }
}
