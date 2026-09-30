//! Shared memory candidate extraction.
//!
//! This module turns persisted tier/user memory into item-level candidate
//! documents. It intentionally does not rank or budget-pack candidates; prompt
//! rendering, search indexes, evals, and diagnostics can all reuse the same
//! canonical extraction path and apply their own retrieval policy.

use std::{path::PathBuf, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::boundary_tags::neutralize_boundary_tags;
use crate::memory_record::V3MemoryTierRecord;
use crate::memory_renderer::{primary_collection_field, DefaultMemoryRenderer};
use crate::memory_tiers::{MemoryRenderer, MemoryTierDefinition, TierScope};
use crate::retrieval_scope::{
    engagement_scope_token, label_for_item, label_from_metadata, stamp_engagement_scope,
    ContextLabel, RetrievalScope, ENGAGEMENT_SCOPE_KEY,
};
use crate::storage_trait::{MemoryStorage, MemoryStorageError};

const ENVIRONMENT_KNOWLEDGE_TIER: &str = "environment_knowledge";
pub const APP_MEMORY_INDEX_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const APP_MEMORY_INDEX_IMPLEMENTATION_DIGEST: &str =
    "blake3:5bda98208d7a13c883e8671967f95ef69da0afc09e6ff2582241397535408f04";
pub const APP_MEMORY_INDEX_PROJECTION_FILE: &str = "app-memory-destination-projection-v1.json";
const APP_MEMORY_INDEX_PROJECTION_MAX_ENTRIES: usize = 4_096;
const APP_MEMORY_INDEX_PROJECTION_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Canonical metadata key for app-sourced memory. Package code cannot assign
/// temperature or force prompt inclusion on documents that carry this envelope.
pub const APP_SOURCE_ELIGIBILITY_METADATA_KEY: &str = "app_source_eligibility";

pub fn memory_candidate_has_app_source_envelope(metadata: &Value) -> bool {
    metadata.get(APP_SOURCE_ELIGIBILITY_METADATA_KEY).is_some()
}

pub fn strip_package_assigned_memory_heat(metadata: &mut Value) {
    if !memory_candidate_has_app_source_envelope(metadata) {
        return;
    }
    if let Some(map) = metadata.as_object_mut() {
        map.remove("temperature");
        map.remove("force_prompt_inclusion");
    }
}

#[derive(Debug, Clone)]
pub struct MemoryCandidateRequest<'a> {
    pub scope: TierScope,
    pub goal_id: Option<&'a str>,
    pub recency_cutoff: Option<Duration>,
    pub include_environment_knowledge: bool,
    /// Engagement containment for this load (§5A.2 of
    /// `docs/plans/2026-08-07-opc-engagements-contextual-authority.md`).
    ///
    /// Deliberately a **required** field with no default. This is the one
    /// canonical extraction path — prompt rendering, the search index, the
    /// `search_memory` tool, evals and diagnostics all reach persisted memory
    /// through it — so every caller is made to state the containment it loads
    /// under. A defaulted field would let a new retrieval surface reach the
    /// whole corpus by simply not mentioning engagements, which is the exact
    /// hole §5A.2 exists to close.
    pub retrieval_scope: RetrievalScope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryCandidateDocument {
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub agent_id: Option<String>,
    pub scope: TierScope,
    pub tier_name: String,
    #[serde(default)]
    pub semantic_memory_type: SemanticMemoryType,
    pub goal_id: Option<String>,
    pub item_key: String,
    pub source_path: Option<PathBuf>,
    pub json_pointer: String,
    pub content_hash: String,
    pub last_updated: DateTime<Utc>,
    pub confidence: Option<f64>,
    pub text: String,
    pub metadata_json: Value,
}

/// Disposable index input owned by the canonical app-memory destination.
///
/// The projection is not app state and contains no bearer authority. Its
/// sealed identities make an accepted destination revision the only source of
/// rows that can enter the shared hybrid index. `local_only` entries may be
/// retained here for crash/replay truth, but the background loader below never
/// releases their text to an embedder; they require the credential-scoped
/// ephemeral path in `memory_index`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryIndexProjectionV1 {
    pub schema_version: u16,
    pub destination_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_receipt_digest: Option<String>,
    pub index_implementation_digest: String,
    pub entries: Vec<AppMemoryIndexProjectionEntryV1>,
    pub projection_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryIndexProjectionEntryV1 {
    pub candidate_id: String,
    pub content_revision: u64,
    pub content_digest: String,
    pub source_head_digest: String,
    pub owner_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_receipt_digest: Option<String>,
    pub model_processing: String,
    pub handling_class: String,
    pub policy_digest: String,
    pub provider_partition_digest: String,
    pub source_authority_identity: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    pub document: MemoryCandidateDocument,
}

impl AppMemoryIndexProjectionV1 {
    pub fn seal(mut self) -> Result<Self, MemoryStorageError> {
        self.projection_digest.clear();
        self.projection_digest = app_memory_index_projection_digest(&self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), MemoryStorageError> {
        if self.schema_version != APP_MEMORY_INDEX_PROJECTION_SCHEMA_VERSION
            || self.index_implementation_digest != APP_MEMORY_INDEX_IMPLEMENTATION_DIGEST
            || self.entries.len() > APP_MEMORY_INDEX_PROJECTION_MAX_ENTRIES
            || self.destination_generation == 0 && self.destination_receipt_digest.is_some()
            || self.destination_generation != 0 && self.destination_receipt_digest.is_none()
            || !is_digest(&self.projection_digest)
        {
            return Err(invalid_app_memory_index_projection());
        }
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > APP_MEMORY_INDEX_PROJECTION_MAX_BYTES
            || app_memory_index_projection_digest(self)? != self.projection_digest
        {
            return Err(invalid_app_memory_index_projection());
        }
        let mut identities = std::collections::BTreeSet::new();
        for entry in &self.entries {
            if entry.candidate_id.trim().is_empty()
                || entry.content_revision == 0
                || entry.owner_generation == 0
                || !is_digest(&entry.content_digest)
                || !is_digest(&entry.source_head_digest)
                || entry
                    .owner_receipt_digest
                    .as_deref()
                    .is_some_and(|value| !is_digest(value))
                || !matches!(
                    entry.model_processing.as_str(),
                    "local_only" | "remote_allowed"
                )
                || entry.handling_class.trim().is_empty()
                || !is_digest(&entry.policy_digest)
                || !is_digest(&entry.provider_partition_digest)
                || !entry.source_authority_identity.is_object()
                || entry.expires_at_ms.is_some_and(|expires| expires < 0)
                || entry.document.item_key != entry.candidate_id
                || entry.document.content_hash != entry.content_digest.trim_start_matches("blake3:")
                || !identities.insert((
                    entry.candidate_id.as_str(),
                    entry.content_revision,
                    entry.source_head_digest.as_str(),
                ))
            {
                return Err(invalid_app_memory_index_projection());
            }
            let Some(identity) = entry.document.metadata_json.get("app_index_identity") else {
                return Err(invalid_app_memory_index_projection());
            };
            if identity.get("source_head_digest").and_then(Value::as_str)
                != Some(entry.source_head_digest.as_str())
                || identity.get("content_revision").and_then(Value::as_u64)
                    != Some(entry.content_revision)
                || identity
                    .get("provider_partition_digest")
                    .and_then(Value::as_str)
                    != Some(entry.provider_partition_digest.as_str())
                || identity.get("source_authority_identity")
                    != Some(&entry.source_authority_identity)
            {
                return Err(invalid_app_memory_index_projection());
            }
        }
        Ok(())
    }
}

pub fn app_memory_index_projection_path(storage: &dyn MemoryStorage) -> PathBuf {
    storage
        .root()
        .join("index")
        .join(APP_MEMORY_INDEX_PROJECTION_FILE)
}

pub async fn write_app_memory_index_projection(
    storage: &dyn MemoryStorage,
    projection: &AppMemoryIndexProjectionV1,
) -> Result<bool, MemoryStorageError> {
    projection.validate()?;
    let path = app_memory_index_projection_path(storage);
    if let Ok(existing) = storage.read_json_value(&path).await {
        if existing.get("projection_digest").and_then(Value::as_str)
            == Some(projection.projection_digest.as_str())
        {
            return Ok(false);
        }
    }
    storage
        .write_json_value_atomic(&path, &serde_json::to_value(projection)?)
        .await?;
    Ok(true)
}

pub async fn load_app_memory_index_projection(
    storage: &dyn MemoryStorage,
) -> Result<Option<AppMemoryIndexProjectionV1>, MemoryStorageError> {
    let path = app_memory_index_projection_path(storage);
    let value = match storage.read_json_value(&path).await {
        Ok(value) => value,
        Err(MemoryStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        },
        Err(error) => return Err(error),
    };
    let projection: AppMemoryIndexProjectionV1 = serde_json::from_value(value)?;
    projection.validate()?;
    Ok(Some(projection))
}

fn app_memory_index_projection_digest(
    projection: &AppMemoryIndexProjectionV1,
) -> Result<String, MemoryStorageError> {
    let mut value = serde_json::to_value(projection)?;
    value["projection_digest"] = Value::String(String::new());
    let bytes = serde_json::to_vec(&value)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.app-memory-index-projection.v1\0");
    hasher.update(&bytes);
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn is_digest(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn invalid_app_memory_index_projection() -> MemoryStorageError {
    MemoryStorageError::Other("invalid app-memory index projection".to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticMemoryType {
    UserPreference,
    AgentContext,
    Procedure,
    Entity,
    Episode,
    Environment,
    ProjectContext,
    SourceEvidence,
    /// Durable, per-project facts about a codebase distilled from coding runs
    /// ("auth lives in X", "we tried W and it broke", "this repo uses pattern Y").
    /// Surfaced to the coding agent via the `magician_code_knowledge` citizen tool.
    CodeKnowledge,
    /// Relationships, opinions, group dynamics, and sentiment gathered from the
    /// social network. Used exclusively during social gating and composing.
    Social,
}

impl Default for SemanticMemoryType {
    fn default() -> Self {
        Self::AgentContext
    }
}

impl SemanticMemoryType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserPreference => "user_preference",
            Self::AgentContext => "agent_context",
            Self::Procedure => "procedure",
            Self::Entity => "entity",
            Self::Episode => "episode",
            Self::Environment => "environment",
            Self::ProjectContext => "project_context",
            Self::SourceEvidence => "source_evidence",
            Self::CodeKnowledge => "code_knowledge",
            Self::Social => "social",
        }
    }

    pub fn prompt_label(self) -> &'static str {
        match self {
            Self::UserPreference => "User Preferences",
            Self::AgentContext => "Agent Context",
            Self::Procedure => "Procedures",
            Self::Entity => "Entities",
            Self::Episode => "Episodes",
            Self::Environment => "Environment",
            Self::ProjectContext => "Project Context",
            Self::SourceEvidence => "Source Evidence",
            Self::CodeKnowledge => "Code Knowledge",
            Self::Social => "Social Dynamics",
        }
    }
}

/// Load persisted memory and split collection tiers into item-level candidates.
pub async fn load_memory_candidate_documents(
    storage: &dyn MemoryStorage,
    agent_id: &str,
    tier_definitions: &[MemoryTierDefinition],
    request: &MemoryCandidateRequest<'_>,
) -> Result<Vec<MemoryCandidateDocument>, MemoryStorageError> {
    let mut candidates = Vec::new();
    if matches!(&request.scope, &TierScope::User) {
        push_user_memory_entries(storage, agent_id, request, &mut candidates).await;
    }

    let renderer = DefaultMemoryRenderer;
    for tier in tier_definitions
        .iter()
        .filter(|tier| same_scope(&tier.scope, &request.scope))
    {
        if matches!(&request.scope, &TierScope::Agent)
            && tier.name == ENVIRONMENT_KNOWLEDGE_TIER
            && !request.include_environment_knowledge
        {
            continue;
        }
        let goal_id = if matches!(&tier.scope, &TierScope::AgentGoal) {
            request.goal_id
        } else {
            None
        };
        let Some(raw) = storage
            .load_native_tier_value(agent_id, &tier.name, &tier.scope, goal_id)
            .await?
        else {
            continue;
        };
        let data: V3MemoryTierRecord = serde_json::from_value(raw)?;
        if is_stale(data.last_updated, request.recency_cutoff) {
            continue;
        }
        if tier_record_is_empty(&data.fields) {
            continue;
        }
        let source_path = storage
            .agent_tier_path(agent_id, &tier.name, &tier.scope, goal_id)
            .ok();
        push_tier_entries(
            storage,
            agent_id,
            tier,
            &data,
            source_path,
            &renderer,
            request,
            &mut candidates,
        );
    }

    // App memory is indexed only from the destination-owned, sealed
    // projection. Entity writes never call this producer. LocalOnly content is
    // intentionally absent from background indexing and receives an
    // independently authorized ephemeral hybrid pass at prompt time.
    if let Some(projection) = load_app_memory_index_projection(storage).await? {
        let now_ms = Utc::now().timestamp_millis();
        candidates.extend(projection.entries.into_iter().filter_map(|entry| {
            if entry.model_processing != "remote_allowed"
                || entry.expires_at_ms.is_some_and(|expiry| now_ms >= expiry)
                || !same_scope(&entry.document.scope, &request.scope)
                || matches!(&request.scope, &TierScope::AgentGoal)
                    && entry.document.goal_id.as_deref() != request.goal_id
                || is_stale(entry.document.last_updated, request.recency_cutoff)
            {
                return None;
            }
            Some(entry.document)
        }));
    }

    // §5A.2 containment, applied once, at the end, on the whole candidate set.
    //
    // Here rather than at each `push_*` site on purpose: a filter that lives
    // on some producers is not a filter, and a new producer added to this
    // function would otherwise ship unfiltered. Every candidate carries a
    // stamped label by this point, so the decision is a pure function of the
    // candidate.
    retain_candidates_for_scope(&request.retrieval_scope, &mut candidates);

    // Lifecycle revisions have stable record identities independent of their
    // human key. Resolve cross-tier successor links only within the admitted set.
    let records: std::collections::HashMap<_, _> = candidates
        .iter()
        .filter_map(|candidate| {
            candidate
                .metadata_json
                .get("memory_record_id")
                .and_then(Value::as_str)
                .map(|id| {
                    (
                        id.to_owned(),
                        crate::memory_temperature::memory_temperature_candidate_key(candidate),
                    )
                })
        })
        .collect();
    for candidate in &mut candidates {
        if let Some(successor) = candidate
            .metadata_json
            .get("superseded_by")
            .and_then(Value::as_str)
            .and_then(|id| records.get(id))
        {
            candidate.metadata_json["superseded_by_candidate_key"] = json!(successor);
        }
    }

    Ok(candidates)
}

/// Drop every candidate the scope does not admit.
///
/// Exposed because two candidate producers live outside this module —
/// episodic memory (`memory/agents/<id>/episodes/*.json`, one file per
/// episode, which the tier walk above never sees) and any surface that
/// assembles `MemoryCandidateDocument`s itself. They must be able to apply the
/// same decision rather than reimplement it, because two implementations of a
/// containment rule are two chances to disagree about the unlabelled case.
pub fn retain_candidates_for_scope(
    scope: &RetrievalScope,
    candidates: &mut Vec<MemoryCandidateDocument>,
) {
    if !scope.is_bound() {
        return;
    }
    candidates.retain(|candidate| scope.admits(&label_from_metadata(&candidate.metadata_json)));
}

/// The label a loaded candidate resolved to. Readers that need the reason for
/// a denial (audit lines, diagnostics) go through this rather than re-reading
/// the raw metadata key.
pub fn candidate_engagement_label(candidate: &MemoryCandidateDocument) -> ContextLabel {
    label_from_metadata(&candidate.metadata_json)
}

/// The engagement label a whole tier record asserts, if any.
///
/// A record-level label is inherited by every item inside it that does not
/// declare one — the container is the natural place to say "this entire tier
/// belongs to one engagement" without labelling a hundred rows.
fn record_scope_token(fields: &std::collections::HashMap<String, Value>) -> Option<String> {
    let token = fields.get(ENGAGEMENT_SCOPE_KEY)?.as_str()?.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

fn push_tier_entries(
    storage: &dyn MemoryStorage,
    agent_id: &str,
    tier: &MemoryTierDefinition,
    data: &V3MemoryTierRecord,
    source_path: Option<PathBuf>,
    renderer: &dyn MemoryRenderer,
    request: &MemoryCandidateRequest<'_>,
    candidates: &mut Vec<MemoryCandidateDocument>,
) {
    let record_token = record_scope_token(&data.fields);
    let inherited = record_token.as_deref();
    let mut pushed_collection_items = false;
    if let Some(collection_field) = primary_collection_field(tier) {
        let collection = data
            .fields
            .get(collection_field)
            .map(|value| (collection_field, value))
            .or_else(|| data.fields.get("value").map(|value| ("value", value)));
        if let Some((source_field, Value::Array(items))) = collection {
            push_collection_items(
                storage,
                agent_id,
                &tier.name,
                collection_field,
                source_field,
                items,
                inherited,
                data.last_updated,
                source_path.clone(),
                "/fields",
                request,
                candidates,
            );
            pushed_collection_items = true;
        }
    }

    for (field, value) in &data.fields {
        if primary_collection_field(tier).is_some_and(|primary| primary == field) {
            continue;
        }
        if field == "value" && pushed_collection_items {
            continue;
        }
        if let Value::Array(items) = value {
            push_collection_items(
                storage,
                agent_id,
                &tier.name,
                field,
                field,
                items,
                inherited,
                data.last_updated,
                source_path.clone(),
                "/fields",
                request,
                candidates,
            );
            pushed_collection_items = true;
        }
    }

    if pushed_collection_items {
        return;
    }

    let rendered = renderer.render(tier, data);
    let rendered = rendered.trim();
    if rendered.is_empty() {
        return;
    }
    let mut tier_metadata = json!({ "candidate_kind": "tier" });
    stamp_engagement_scope(&mut tier_metadata, &label_for_item(&Value::Null, inherited));
    candidates.push(build_candidate(
        storage,
        Some(agent_id),
        request.scope.clone(),
        &tier.name,
        request.goal_id,
        "tier",
        source_path,
        "/fields".to_string(),
        data.last_updated,
        None,
        neutralize_boundary_tags(rendered),
        tier_metadata,
    ));
}

#[allow(clippy::too_many_arguments)]
fn push_collection_items(
    storage: &dyn MemoryStorage,
    agent_id: &str,
    tier_name: &str,
    field: &str,
    source_field: &str,
    items: &[Value],
    inherited_engagement_scope: Option<&str>,
    last_updated: DateTime<Utc>,
    source_path: Option<PathBuf>,
    pointer_prefix: &str,
    request: &MemoryCandidateRequest<'_>,
    candidates: &mut Vec<MemoryCandidateDocument>,
) {
    for (idx, item) in items.iter().enumerate() {
        if value_is_empty(item) {
            continue;
        }
        let item_key = item_memory_key(item).unwrap_or_else(|| idx.to_string());
        let text = format!("{field}: {}", value_to_prompt_text(item));
        let confidence = item_confidence(item);
        let tier_label = format!("{tier_name}.{field}");
        let mut metadata = json!({
            "candidate_kind": "collection_item",
            "field": field,
            "source_field": source_field,
            "index": idx,
            "project_id": item.get("project_id").and_then(Value::as_str),
            "memory_lifecycle": item_lifecycle(item),
            "memory_record_id": item.get("memory_record_id"),
            "valid_until": item.get("valid_until"),
            "superseded_by": item.get("superseded_by").and_then(Value::as_str),
            "superseded_at": item.get("superseded_at").and_then(Value::as_str),
            "supersession_reason": item.get("supersession_reason").and_then(Value::as_str),
            "supersession_confidence": item.get("supersession_confidence").and_then(Value::as_f64),
            "supersession_source": item.get("supersession_source").and_then(Value::as_str),
        });
        copy_app_source_envelope(item, &mut metadata);
        stamp_engagement_scope(
            &mut metadata,
            &label_for_item(item, inherited_engagement_scope),
        );
        candidates.push(build_candidate(
            storage,
            Some(agent_id),
            request.scope.clone(),
            &tier_label,
            request.goal_id,
            &item_key,
            source_path.clone(),
            format!(
                "{}/{}/{}",
                pointer_prefix.trim_end_matches('/'),
                json_pointer_escape(source_field),
                idx
            ),
            last_updated,
            confidence,
            neutralize_boundary_tags(&text),
            metadata,
        ));
    }
}

async fn push_user_memory_entries(
    storage: &dyn MemoryStorage,
    agent_id: &str,
    request: &MemoryCandidateRequest<'_>,
    candidates: &mut Vec<MemoryCandidateDocument>,
) {
    let now = Utc::now();
    if let Ok(knowledge) = storage.load_user_knowledge().await {
        push_user_value_entries(
            storage,
            agent_id,
            "knowledge",
            &knowledge,
            storage.user_knowledge_path(),
            now,
            request,
            candidates,
        );
    }

    for file_name in ["contacts.json", "routines.json", "research_findings.json"] {
        let path = storage.user_root().join(file_name);
        if let Ok(value) = storage.read_json_value(&path).await {
            let source = file_name.trim_end_matches(".json");
            push_user_value_entries(
                storage, agent_id, source, &value, path, now, request, candidates,
            );
        }
    }
}

fn push_user_value_entries(
    storage: &dyn MemoryStorage,
    agent_id: &str,
    source: &str,
    value: &Value,
    source_path: PathBuf,
    last_updated: DateTime<Utc>,
    request: &MemoryCandidateRequest<'_>,
    candidates: &mut Vec<MemoryCandidateDocument>,
) {
    let field_root = value.get("fields").unwrap_or(value);
    let pointer_root = if value.get("fields").is_some() {
        "/fields"
    } else {
        ""
    };
    // A user memory file may declare one engagement for the whole file, at the
    // top level or inside `fields`. Either spelling is a container label that
    // its entries inherit unless they carry their own.
    let record_token = engagement_scope_token(value).or_else(|| engagement_scope_token(field_root));
    let inherited = record_token.as_deref();
    let Some(map) = field_root.as_object() else {
        return;
    };
    for (field, field_value) in map {
        if field.starts_with('_') {
            continue;
        }
        let prompt_field = if field == "value" && source != "knowledge" {
            source
        } else {
            field
        };
        match field_value {
            Value::Array(items) => {
                push_collection_items(
                    storage,
                    agent_id,
                    source,
                    prompt_field,
                    field,
                    items,
                    inherited,
                    last_updated,
                    Some(source_path.clone()),
                    pointer_root,
                    request,
                    candidates,
                );
            },
            other if !value_is_empty(other) => {
                let text = format!("{prompt_field}: {}", value_to_prompt_text(other));
                let confidence = item_confidence(other);
                let tier_label = format!("{source}.{prompt_field}");
                let mut metadata = json!({
                    "candidate_kind": "user_field",
                    "field": field,
                });
                copy_app_source_envelope(other, &mut metadata);
                stamp_engagement_scope(&mut metadata, &label_for_item(other, inherited));
                candidates.push(build_candidate(
                    storage,
                    Some(agent_id),
                    request.scope.clone(),
                    &tier_label,
                    request.goal_id,
                    prompt_field,
                    Some(source_path.clone()),
                    format!("{}/{}", pointer_root, json_pointer_escape(field)),
                    last_updated,
                    confidence,
                    neutralize_boundary_tags(&text),
                    metadata,
                ));
            },
            _ => {},
        }
    }
}

fn copy_app_source_envelope(source: &Value, metadata: &mut Value) {
    let Some(envelope) = source.get(APP_SOURCE_ELIGIBILITY_METADATA_KEY) else {
        return;
    };
    let Some(map) = metadata.as_object_mut() else {
        return;
    };
    map.insert(
        APP_SOURCE_ELIGIBILITY_METADATA_KEY.to_owned(),
        envelope.clone(),
    );
    // The system owns temperature once a source-linked envelope is present.
    map.remove("temperature");
    map.remove("force_prompt_inclusion");
}

fn build_candidate(
    storage: &dyn MemoryStorage,
    agent_id: Option<&str>,
    scope: TierScope,
    tier_name: &str,
    goal_id: Option<&str>,
    item_key: &str,
    source_path: Option<PathBuf>,
    json_pointer: String,
    last_updated: DateTime<Utc>,
    confidence: Option<f64>,
    text: String,
    metadata_json: Value,
) -> MemoryCandidateDocument {
    let (principal, workspace) = storage
        .scope_segments()
        .map(|(principal, workspace)| (Some(principal), Some(workspace)))
        .unwrap_or((None, None));
    let content_hash = blake3::hash(text.as_bytes()).to_hex().to_string();
    let candidate_agent_id = if matches!(&scope, &TierScope::User) {
        None
    } else {
        agent_id.map(ToString::to_string)
    };
    let semantic_memory_type = infer_semantic_memory_type(&scope, tier_name, &metadata_json);
    let metadata_json = metadata_with_semantic_memory_type(metadata_json, semantic_memory_type);
    MemoryCandidateDocument {
        principal,
        workspace,
        agent_id: candidate_agent_id,
        scope,
        tier_name: tier_name.to_string(),
        semantic_memory_type,
        goal_id: goal_id.map(ToString::to_string),
        item_key: item_key.to_string(),
        source_path,
        json_pointer,
        content_hash,
        last_updated,
        confidence,
        text,
        metadata_json,
    }
}

pub fn infer_semantic_memory_type(
    scope: &TierScope,
    tier_name: &str,
    metadata_json: &Value,
) -> SemanticMemoryType {
    if metadata_json
        .get("candidate_kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("episode"))
    {
        return SemanticMemoryType::Episode;
    }

    let name = tier_name.trim().to_ascii_lowercase();
    if contains_any(&name, &["episode", "recent_activity"]) {
        return SemanticMemoryType::Episode;
    }
    if contains_any(
        &name,
        &[
            "archive",
            "audit",
            "evidence",
            "research_findings",
            "screen_observations",
            "source",
        ],
    ) {
        return SemanticMemoryType::SourceEvidence;
    }
    if name.contains("environment") {
        return SemanticMemoryType::Environment;
    }
    if contains_any(
        &name,
        &[
            "workflow",
            "workflows",
            "routine",
            "routines",
            "skill",
            "skills",
            "procedure",
            "procedures",
            "strategy",
            "strategies",
        ],
    ) {
        return SemanticMemoryType::Procedure;
    }
    if contains_any(
        &name,
        &[
            "account",
            "accounts",
            "channel",
            "channels",
            "contact",
            "contacts",
            "entity",
            "entities",
            "organization",
            "organizations",
        ],
    ) {
        return SemanticMemoryType::Entity;
    }
    // Code knowledge must be checked BEFORE the user-preference block, since
    // "code_knowledge" / "architectural_knowledge" both contain "knowledge" and
    // would otherwise map to UserPreference. Architectural knowledge (system
    // boundaries, API contracts, design decisions) IS project code knowledge for
    // a coding agent, so it shares the CodeKnowledge lane. The generic "coding"
    // signal stays ProjectContext below.
    if contains_any(
        &name,
        &["code_knowledge", "codebase", "source_code", "architectural"],
    ) {
        return SemanticMemoryType::CodeKnowledge;
    }
    if contains_any(
        &name,
        &[
            "preference",
            "preferences",
            "identity",
            "profile",
            "knowledge",
            "personal",
        ],
    ) {
        return SemanticMemoryType::UserPreference;
    }
    if contains_any(
        &name,
        &[
            "project",
            "task_progress",
            "goal",
            "milestone",
            "workspace_context",
            "coding",
        ],
    ) || matches!(scope, &TierScope::AgentGoal)
    {
        return SemanticMemoryType::ProjectContext;
    }

    match scope {
        TierScope::User => SemanticMemoryType::UserPreference,
        TierScope::Agent => SemanticMemoryType::AgentContext,
        TierScope::AgentGoal => SemanticMemoryType::ProjectContext,
    }
}

fn metadata_with_semantic_memory_type(
    mut metadata_json: Value,
    semantic_memory_type: SemanticMemoryType,
) -> Value {
    if let Some(map) = metadata_json.as_object_mut() {
        map.entry("semantic_memory_type".to_string())
            .or_insert_with(|| Value::String(semantic_memory_type.as_str().to_string()));
        if map.contains_key(APP_SOURCE_ELIGIBILITY_METADATA_KEY) {
            map.remove("temperature");
            map.remove("force_prompt_inclusion");
        }
        return metadata_json;
    }
    json!({
        "semantic_memory_type": semantic_memory_type.as_str(),
        "value": metadata_json,
    })
}

fn same_scope(left: &TierScope, right: &TierScope) -> bool {
    matches!(
        (left, right),
        (&TierScope::User, &TierScope::User)
            | (&TierScope::Agent, &TierScope::Agent)
            | (&TierScope::AgentGoal, &TierScope::AgentGoal)
    )
}

fn is_stale(last_updated: DateTime<Utc>, cutoff: Option<Duration>) -> bool {
    let Some(cutoff) = cutoff else {
        return false;
    };
    let Ok(cutoff) = chrono::Duration::from_std(cutoff) else {
        return false;
    };
    Utc::now().signed_duration_since(last_updated) > cutoff
}

fn tier_record_is_empty(fields: &std::collections::HashMap<String, Value>) -> bool {
    fields.values().all(value_is_empty)
}

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(_) | Value::Number(_) => false,
        Value::String(value) => value.trim().is_empty(),
        Value::Array(values) => values.iter().all(value_is_empty),
        Value::Object(values) => values.values().all(value_is_empty),
    }
}

fn value_to_prompt_text(value: &Value) -> String {
    if value.get("memory_record_id").is_some() {
        // Evidence/history supports review, not a second set of instructions in
        // ordinary recall. Present current meaning and its applicability only.
        let meaning = value.get("value").cloned().unwrap_or_else(|| {
            Value::Object(
                value
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.iter())
                    .filter(|(key, _)| {
                        !key.starts_with("memory_")
                            && !key.starts_with("supersed")
                            && !matches!(
                                key.as_str(),
                                "source_ids" | "source_event_id" | "root_source_id" | "rationale"
                            )
                    })
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
        });
        return json!({"key":value.get("key"),"value":meaning,
            "kind":value.get("memory_kind"),"context":value.get("memory_context"),
            "valid_until":value.get("valid_until"),
            "uncertainty":if item_lifecycle(value)==Some("unresolved") {
                Some("Unresolved memory conflict. Do not treat this claim as settled current truth.")
            } else {None}}).to_string();
    }
    match value {
        Value::String(text) => text.clone(),
        Value::Number(_) | Value::Bool(_) | Value::Null => value.to_string(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
        },
    }
}

/// The item segment of a candidate key for one collection item.
///
/// Public because supersession has two sides: the consolidator names the
/// replacement, and the candidate loader names the same item when it builds
/// candidates. Those two names must agree exactly or a supersession chain
/// dangles, so they come from this one function rather than from two
/// derivations that can drift.
///
/// `None` means the item carries no stable identity. Callers must not
/// substitute the array index — `push_collection_items` does so only because
/// it is naming an item it is looking at right now, and that index is not
/// stable across a merge.
pub fn item_memory_key(value: &Value) -> Option<String> {
    if let Some(id) = value
        .get("memory_record_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        return Some(format!("record:{id}"));
    }
    if memory_item_is_superseded(value) {
        if let Some(raw) = value.get("superseded_id").and_then(Value::as_str) {
            let normalized = normalize_item_key_part(raw);
            if !normalized.is_empty() {
                return Some(format!("superseded:{normalized}"));
            }
        }
    }
    for field in [
        "key",
        "environment_key",
        "pattern",
        "insight",
        "name",
        "source_id",
        "id",
    ] {
        if let Some(raw) = value.get(field).and_then(Value::as_str) {
            let normalized = raw
                .split_whitespace()
                .map(|part| part.to_lowercase())
                .collect::<Vec<_>>()
                .join(" ");
            if !normalized.is_empty() {
                return Some(format!("{field}:{normalized}"));
            }
        }
    }
    None
}

fn memory_item_is_superseded(value: &Value) -> bool {
    value
        .get("memory_lifecycle")
        .or_else(|| value.get("lifecycle"))
        .or_else(|| value.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "superseded" | "replaced"
            )
        })
}

fn item_lifecycle(value: &Value) -> Option<&str> {
    value
        .get("memory_lifecycle")
        .or_else(|| value.get("lifecycle"))
        .or_else(|| value.get("status"))
        .and_then(Value::as_str)
}

fn normalize_item_key_part(raw: &str) -> String {
    raw.split_whitespace()
        .map(|part| part.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn item_confidence(value: &Value) -> Option<f64> {
    value.get("confidence").and_then(Value::as_f64)
}

fn json_pointer_escape(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

// Tests that exercise `load_memory_candidate_documents` against magician's
// concrete `AgentMemoryService` / `AgentStorage` live as integration tests
// in `magician/tests/memory_index_vector_index_integration.rs` so this
// crate has no dev-time dependency on magician.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app_projection_entry(model_processing: &str) -> AppMemoryIndexProjectionEntryV1 {
        let text = "[App-proposed hypothesis] Asha is a mentor".to_owned();
        let content_hash = blake3::hash(text.as_bytes()).to_hex().to_string();
        let source_head_digest = format!("blake3:{}", blake3::hash(b"source-head").to_hex());
        let provider_partition_digest =
            format!("blake3:{}", blake3::hash(b"provider-partition").to_hex());
        AppMemoryIndexProjectionEntryV1 {
            candidate_id: "memory:candidate:1".to_owned(),
            content_revision: 2,
            content_digest: format!("blake3:{content_hash}"),
            source_head_digest: source_head_digest.clone(),
            owner_generation: 2,
            owner_receipt_digest: None,
            model_processing: model_processing.to_owned(),
            handling_class: "personal".to_owned(),
            policy_digest: format!("blake3:{}", blake3::hash(b"policy").to_hex()),
            provider_partition_digest: provider_partition_digest.clone(),
            source_authority_identity: json!({
                "contract": "legacy-app-memory-candidate-v1",
                "contribution_port_id": "legacy-memory-candidate-store",
            }),
            expires_at_ms: None,
            document: MemoryCandidateDocument {
                principal: Some("anonymous".to_owned()),
                workspace: Some("default".to_owned()),
                agent_id: None,
                scope: TierScope::User,
                tier_name: "app.entities".to_owned(),
                semantic_memory_type: SemanticMemoryType::Entity,
                goal_id: None,
                item_key: "memory:candidate:1".to_owned(),
                source_path: None,
                json_pointer: "/app_memory_candidates/memory:candidate:1".to_owned(),
                content_hash,
                last_updated: Utc::now(),
                confidence: None,
                text,
                metadata_json: json!({
                    APP_SOURCE_ELIGIBILITY_METADATA_KEY: {
                        "candidate_id": "memory:candidate:1"
                    },
                    "app_index_identity": {
                        "content_revision": 2,
                        "source_head_digest": source_head_digest,
                        "provider_partition_digest": provider_partition_digest,
                        "source_authority_identity": {
                            "contract": "legacy-app-memory-candidate-v1",
                            "contribution_port_id": "legacy-memory-candidate-store",
                        },
                    }
                }),
            },
        }
    }

    #[test]
    fn destination_projection_seal_binds_source_head_and_provider_partition() {
        let projection = AppMemoryIndexProjectionV1 {
            schema_version: APP_MEMORY_INDEX_PROJECTION_SCHEMA_VERSION,
            destination_generation: 0,
            destination_receipt_digest: None,
            index_implementation_digest: APP_MEMORY_INDEX_IMPLEMENTATION_DIGEST.to_owned(),
            entries: vec![app_projection_entry("remote_allowed")],
            projection_digest: String::new(),
        }
        .seal()
        .expect("sealed destination projection");
        projection.validate().expect("valid sealed projection");

        let mut substituted_source = projection.clone();
        substituted_source.entries[0].source_head_digest =
            format!("blake3:{}", blake3::hash(b"substituted-source").to_hex());
        assert!(substituted_source.validate().is_err());

        let mut substituted_provider = projection;
        substituted_provider.entries[0].provider_partition_digest =
            format!("blake3:{}", blake3::hash(b"substituted-provider").to_hex());
        assert!(substituted_provider.validate().is_err());
    }

    /// An item's key changes the moment it is marked superseded, because the
    /// superseded row and the replacement that reuses its `key` field must not
    /// collide.
    ///
    /// This is what bounds supersession chain depth. A pointer recorded as
    /// "replaced by item X" resolves while X is live; once X is itself
    /// superseded, X's own key becomes the `superseded:` form and the earlier
    /// pointer no longer names it. A three-deep chain therefore resolves its
    /// first hop and reports the second as missing — strictly better than the
    /// previous behaviour, where no hop resolved at all, but not unlimited.
    ///
    /// Pinned here so the bound is a known property rather than a surprise.
    #[test]
    fn an_items_key_changes_once_it_is_marked_superseded() {
        let live = json!({ "key": "coffee_current", "value": "likes coke zero" });
        let live_key = item_memory_key(&live).expect("live item has a key");
        assert_eq!(live_key, "key:coffee_current");

        let superseded = json!({
            "key": "coffee_current",
            "value": "likes coke zero",
            "memory_lifecycle": "superseded",
            "superseded_id": "durable::key::coffee current::abc123",
        });
        let superseded_key = item_memory_key(&superseded).expect("superseded item has a key");
        assert!(
            superseded_key.starts_with("superseded:"),
            "got {superseded_key}"
        );
        assert_ne!(live_key, superseded_key);
    }

    /// No stable identity means no key. Callers must not invent one.
    #[test]
    fn an_item_without_identifying_fields_has_no_key() {
        assert_eq!(item_memory_key(&json!({ "note": "an observation" })), None);
    }

    #[test]
    fn app_source_envelope_survives_item_projection_without_package_heat() {
        let source = json!({
            APP_SOURCE_ELIGIBILITY_METADATA_KEY: { "candidate_id": "candidate:1" },
            "temperature": "hot",
            "force_prompt_inclusion": true,
        });
        let mut metadata = json!({
            "candidate_kind": "collection_item",
            "temperature": "package_owned",
            "force_prompt_inclusion": true,
        });

        copy_app_source_envelope(&source, &mut metadata);

        assert_eq!(
            metadata.get(APP_SOURCE_ELIGIBILITY_METADATA_KEY),
            source.get(APP_SOURCE_ELIGIBILITY_METADATA_KEY)
        );
        assert!(metadata.get("temperature").is_none());
        assert!(metadata.get("force_prompt_inclusion").is_none());
    }

    #[test]
    fn infer_semantic_memory_type_preserves_typed_lanes() {
        assert_eq!(
            infer_semantic_memory_type(&TierScope::User, "preferences.items", &json!({})),
            SemanticMemoryType::UserPreference
        );
        assert_eq!(
            infer_semantic_memory_type(&TierScope::User, "routines.items", &json!({})),
            SemanticMemoryType::Procedure
        );
        assert_eq!(
            infer_semantic_memory_type(&TierScope::User, "contacts.items", &json!({})),
            SemanticMemoryType::Entity
        );
        assert_eq!(
            infer_semantic_memory_type(&TierScope::Agent, "environment_knowledge", &json!({})),
            SemanticMemoryType::Environment
        );
        assert_eq!(
            infer_semantic_memory_type(&TierScope::AgentGoal, "task_progress.notes", &json!({})),
            SemanticMemoryType::ProjectContext
        );
        // `code_knowledge` must win over the user-preference "knowledge" keyword.
        assert_eq!(
            infer_semantic_memory_type(&TierScope::Agent, "code_knowledge.facts", &json!({})),
            SemanticMemoryType::CodeKnowledge
        );
        // The engineer agents' own code tiers route to the CodeKnowledge lane:
        // `codebase_knowledge` (senior-dev) and `architectural_knowledge`
        // (principal) — the latter also contains "knowledge" but must NOT fall
        // through to UserPreference.
        assert_eq!(
            infer_semantic_memory_type(&TierScope::Agent, "codebase_knowledge.entries", &json!({})),
            SemanticMemoryType::CodeKnowledge
        );
        assert_eq!(
            infer_semantic_memory_type(
                &TierScope::Agent,
                "architectural_knowledge.entries",
                &json!({})
            ),
            SemanticMemoryType::CodeKnowledge
        );
        // The generic "coding" signal stays ProjectContext (not a code lane).
        assert_eq!(
            infer_semantic_memory_type(&TierScope::AgentGoal, "coding_task", &json!({})),
            SemanticMemoryType::ProjectContext
        );
        assert_eq!(
            infer_semantic_memory_type(
                &TierScope::Agent,
                "anything",
                &json!({ "candidate_kind": "episode" })
            ),
            SemanticMemoryType::Episode
        );
    }
}
