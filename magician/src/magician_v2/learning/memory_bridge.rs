use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Map, Value};
use tracing::warn;

use crate::magician_v2::{
    agents::{storage::AgentStorage, AgentMemoryService},
    analytics::memory_parquet::{emit_rows_for_storage, json_payload, MemoryAnalyticsRow},
    artifact_v2::workspace::ArtifactV2Workspace,
};

use super::{
    CreateLearningEventRequest, LearningCandidate, LearningCandidateState, LearningCandidateType,
    LearningEvidenceRef, LearningRiskLevel, LearningScope, LearningStore,
};

const MIN_EXPLICIT_MEMORY_CONFIDENCE: f64 = 0.70;
// No per-tier size cap here. This bridge used to drain the oldest entries
// above 500, which silently deleted owner memory (a 2,097-entry `workflows`
// tier would have lost ~1,600 records on its next promotion). Section sizes
// are bounded centrally on every `knowledge.json` write, archive first:
// `agents::user_memory_bounds` (docs/archive/plans/2026-10-18-user-memory-growth-bounds.md).
/// Promotion audit rows kept in `knowledge.json`. Every row is also appended
/// to `memory/users/archive/learning_promotions.jsonl`, so trimming loses
/// nothing; under `memory.user_knowledge_bounds` enforce the main file keeps
/// only the last [`ENFORCED_PROMOTION_AUDIT_ITEMS`].
const MAX_PROMOTION_AUDIT_ITEMS: usize = 200;
const ENFORCED_PROMOTION_AUDIT_ITEMS: usize = 20;
const USER_MEMORY_KNOWLEDGE_RELATIVE_PATH: &str = "memory/users/knowledge.json";
const LEARNING_MEMORY_BRIDGE_SOURCE: &str = "learning_memory_bridge";

#[derive(Debug, Clone, Serialize)]
pub struct LearningMemoryRouteOutcome {
    pub candidate_id: String,
    pub routed: bool,
    pub promoted: bool,
    pub target: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MemoryOperation {
    Upsert,
    Replace,
    Remove,
}

#[derive(Debug, Clone)]
struct MemoryPromotionSpec {
    target_scope: String,
    target_tier: String,
    operation: MemoryOperation,
    key: String,
    value: Value,
    explicit_user_request: bool,
    explicit_user_correction: bool,
    replaces_key: Option<String>,
}

#[derive(Debug, Clone)]
struct UserMemoryWriteOutcome {
    target: String,
    superseded_count: usize,
    removed_count: usize,
}

#[derive(Debug, Clone)]
pub struct LearningMemoryBridge {
    workspace_layout: ArtifactV2Workspace,
}

impl LearningMemoryBridge {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub async fn route_candidate(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
    ) -> Result<LearningMemoryRouteOutcome> {
        if candidate.candidate_type.is_procedure_candidate() {
            return Ok(LearningMemoryRouteOutcome {
                candidate_id: candidate.id.clone(),
                routed: false,
                promoted: false,
                target: None,
                reason: "memory_procedure_candidates_route_through_learning_procedure_bridge"
                    .to_string(),
            });
        }
        if !candidate.candidate_type.is_memory_candidate() {
            return Ok(LearningMemoryRouteOutcome {
                candidate_id: candidate.id.clone(),
                routed: false,
                promoted: false,
                target: None,
                reason: "not_a_memory_candidate".to_string(),
            });
        }

        let spec = match MemoryPromotionSpec::from_candidate(candidate) {
            Ok(spec) => spec,
            Err(error) => {
                let reason = format!("memory_candidate_requires_review: {error}");
                self.triage_for_memory_review(store, scope, candidate, &reason, None)
                    .await?;
                return Ok(LearningMemoryRouteOutcome {
                    candidate_id: candidate.id.clone(),
                    routed: true,
                    promoted: false,
                    target: None,
                    reason,
                });
            },
        };
        let target = Some(format!("{}.{}", spec.target_scope, spec.target_tier));

        if let Some(reason) = auto_promotion_blocker(candidate, &spec) {
            self.triage_for_memory_review(store, scope, candidate, &reason, target.clone())
                .await?;
            return Ok(LearningMemoryRouteOutcome {
                candidate_id: candidate.id.clone(),
                routed: true,
                promoted: false,
                target,
                reason,
            });
        }

        let memory_service = AgentMemoryService::with_scoped_memory_scope(
            self.workspace_layout
                .memory_root(&scope.principal, &scope.workspace),
            &scope.principal,
            &scope.workspace,
        );
        let write = self
            .write_user_memory(&memory_service, candidate, &spec)
            .await
            .with_context(|| {
                format!(
                    "promoting learning candidate `{}` to user memory",
                    candidate.id
                )
            })?;

        let evidence = memory_write_evidence(&write, &spec);
        let event = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_memory_candidate_promoted".to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: format!(
                    "Learning candidate `{}` promoted to {}.",
                    candidate.id, write.target
                ),
                evidence_refs: {
                    let mut refs = candidate.evidence_refs.clone();
                    refs.push(evidence.clone());
                    refs
                },
                payload: json!({
                    "candidate_id": candidate.id,
                    "candidate_type": candidate.candidate_type.as_str(),
                    "target": write.target,
                    "operation": spec.operation.as_str(),
                    "key": spec.key,
                    "superseded_count": write.superseded_count,
                    "removed_count": write.removed_count,
                }),
            },
        )?;
        let promoted = store.transition_candidate(
            scope,
            &candidate.id,
            LearningCandidateState::Promoted,
            "learning_memory_bridge",
            "promoted_to_user_memory",
            format!(
                "Explicit low-risk user memory candidate was promoted to {}; learning event {} records the write.",
                write.target, event.id
            ),
            vec![evidence],
        )?;

        emit_memory_route_row(
            &memory_service,
            &promoted,
            "learning_memory_candidate_promoted",
            "promoted",
            Some(&write.target),
            Some(&spec),
            json!({
                "superseded_count": write.superseded_count,
                "removed_count": write.removed_count,
            }),
        );

        Ok(LearningMemoryRouteOutcome {
            candidate_id: promoted.id,
            routed: true,
            promoted: true,
            target: Some(write.target),
            reason: "promoted_explicit_user_memory".to_string(),
        })
    }

    pub async fn promote_reviewed_candidate(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
        actor: &str,
        reason: &str,
    ) -> Result<LearningCandidate> {
        if candidate.candidate_type.is_procedure_candidate() {
            return Err(anyhow!(
                "candidate `{}` is memory_procedure; promote it through the learning procedure bridge",
                candidate.id
            ));
        }
        if !candidate.candidate_type.is_memory_candidate() {
            return Err(anyhow!(
                "candidate `{}` is `{}`; only memory candidates can be promoted through the memory bridge",
                candidate.id,
                candidate.candidate_type.as_str()
            ));
        }
        if candidate.state.is_terminal() {
            return Err(anyhow!(
                "candidate `{}` is terminal in state `{}` and cannot be promoted through the memory bridge",
                candidate.id,
                candidate.state.as_str()
            ));
        }
        let spec = MemoryPromotionSpec::from_candidate(candidate)?;
        if spec.target_scope != "user" {
            return Err(anyhow!(
                "reviewed memory promotion currently supports only user memory; target scope was `{}`",
                spec.target_scope
            ));
        }
        if !crate::magician_v2::chat::service::is_curated_user_memory_tier(&spec.target_tier) {
            return Err(anyhow!(
                "target user tier `{}` is not in the allowlist",
                spec.target_tier
            ));
        }
        if contains_secretish_value(&spec.value)
            || contains_secretish_text(&spec.key)
            || spec
                .replaces_key
                .as_deref()
                .is_some_and(contains_secretish_text)
        {
            return Err(anyhow!(
                "candidate `{}` may contain secret-like material and cannot be promoted automatically",
                candidate.id
            ));
        }

        let memory_service = AgentMemoryService::with_scoped_memory_scope(
            self.workspace_layout
                .memory_root(&scope.principal, &scope.workspace),
            &scope.principal,
            &scope.workspace,
        );
        let write = self
            .write_user_memory(&memory_service, candidate, &spec)
            .await
            .with_context(|| {
                format!(
                    "promoting reviewed learning candidate `{}` to user memory",
                    candidate.id
                )
            })?;
        let evidence = memory_write_evidence(&write, &spec);
        let event = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_memory_candidate_review_promoted".to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: format!(
                    "Reviewed learning candidate `{}` promoted to {} by {}.",
                    candidate.id, write.target, actor
                ),
                evidence_refs: {
                    let mut refs = candidate.evidence_refs.clone();
                    refs.push(evidence.clone());
                    refs
                },
                payload: json!({
                    "candidate_id": candidate.id,
                    "candidate_type": candidate.candidate_type.as_str(),
                    "target": write.target,
                    "operation": spec.operation.as_str(),
                    "key": spec.key,
                    "actor": actor,
                    "reason": reason,
                    "superseded_count": write.superseded_count,
                    "removed_count": write.removed_count,
                }),
            },
        )?;

        let promoted = store.transition_candidate(
            scope,
            &candidate.id,
            LearningCandidateState::Promoted,
            actor,
            "reviewed_promoted_to_user_memory",
            format!(
                "{} Learning event {} records the reviewed memory write.",
                reason, event.id
            ),
            vec![evidence],
        )?;
        emit_memory_route_row(
            &memory_service,
            &promoted,
            "learning_memory_candidate_review_promoted",
            "promoted",
            Some(&write.target),
            Some(&spec),
            json!({
                "actor": actor,
                "reason": reason,
                "superseded_count": write.superseded_count,
                "removed_count": write.removed_count,
            }),
        );
        Ok(promoted)
    }

    async fn triage_for_memory_review(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
        reason: &str,
        target: Option<String>,
    ) -> Result<()> {
        let memory_service = AgentMemoryService::with_scoped_memory_scope(
            self.workspace_layout
                .memory_root(&scope.principal, &scope.workspace),
            &scope.principal,
            &scope.workspace,
        );
        let event = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_memory_candidate_triaged".to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: format!(
                    "Learning memory candidate `{}` routed for review: {}",
                    candidate.id, reason
                ),
                evidence_refs: candidate.evidence_refs.clone(),
                payload: json!({
                    "candidate_id": candidate.id,
                    "candidate_type": candidate.candidate_type.as_str(),
                    "target": target,
                    "reason": reason,
                }),
            },
        )?;

        if matches!(
            candidate.state,
            LearningCandidateState::Observed | LearningCandidateState::Proposed
        ) {
            let _ = store.transition_candidate(
                scope,
                &candidate.id,
                LearningCandidateState::Triaged,
                "learning_memory_bridge",
                "routed_to_memory_review",
                format!(
                    "Memory candidate is not eligible for automatic promotion: {reason}. Route event: {}.",
                    event.id
                ),
                candidate.evidence_refs.clone(),
            )?;
        }

        emit_memory_route_row(
            &memory_service,
            candidate,
            "learning_memory_candidate_triaged",
            "review_required",
            target.as_deref(),
            None,
            json!({ "reason": reason }),
        );
        Ok(())
    }

    async fn write_user_memory(
        &self,
        memory_service: &AgentMemoryService,
        candidate: &LearningCandidate,
        spec: &MemoryPromotionSpec,
    ) -> Result<UserMemoryWriteOutcome> {
        let _flock = AgentStorage::acquire_file_lock_exclusive(
            &memory_service.storage().user_knowledge_path(),
        )
        .await?;
        let mut knowledge = memory_service.load_user_knowledge().await?;
        if !knowledge.is_object() {
            knowledge = Value::Object(Map::new());
        }

        let now = Utc::now().to_rfc3339();
        let mut superseded = Vec::new();
        let mut removed_count = 0usize;
        {
            let root = knowledge
                .as_object_mut()
                .ok_or_else(|| anyhow!("user knowledge root was not an object"))?;
            let tier_value = root
                .entry(spec.target_tier.clone())
                .or_insert_with(|| Value::Array(Vec::new()));
            if !tier_value.is_array() {
                *tier_value = Value::Array(Vec::new());
            }
            let items = tier_value.as_array_mut().ok_or_else(|| {
                anyhow!("user memory tier `{}` is not an array", spec.target_tier)
            })?;

            let normalized_key = normalize_memory_key(&spec.key);
            let normalized_replace = spec.replaces_key.as_deref().map(normalize_memory_key);
            items.retain(|item| {
                let item_key = item
                    .get("key")
                    .and_then(Value::as_str)
                    .map(normalize_memory_key);
                let matches_key = item_key.as_deref() == Some(normalized_key.as_str())
                    || normalized_replace
                        .as_deref()
                        .is_some_and(|replace| item_key.as_deref() == Some(replace));
                if matches_key {
                    superseded.push(item.clone());
                    removed_count += 1;
                    false
                } else {
                    true
                }
            });

            if !matches!(spec.operation, MemoryOperation::Remove) {
                items.push(json!({
                    "key": normalized_key,
                    "value": spec.value.clone(),
                    "confidence": candidate.confidence.unwrap_or(MIN_EXPLICIT_MEMORY_CONFIDENCE),
                    "rationale": candidate.rationale,
                    "source_id": candidate.id,
                    "source_type": "learning_candidate",
                    "target_tier": spec.target_tier,
                    "updated_at": now,
                    "evidence_refs": candidate.evidence_refs.clone(),
                    "learning": {
                        "candidate_type": candidate.candidate_type.as_str(),
                        "explicit_user_request": spec.explicit_user_request,
                        "explicit_user_correction": spec.explicit_user_correction,
                        "operation": spec.operation.as_str(),
                    }
                }));
            }
        }

        // The records this promotion replaces go to the restorable archive
        // (all of them; the audit row keeps at most five copies).
        let bounds = crate::magician_v2::agents::user_memory_bounds::current_settings();
        let archived_at = Utc::now();
        let replaced: Vec<_> = superseded
            .iter()
            .map(|item| {
                crate::magician_v2::agents::user_memory_bounds::archive::ArchiveEntry::new(
                    &spec.target_tier,
                    "learning_promotion_replaced",
                    item.clone(),
                    None,
                    archived_at,
                )
            })
            .collect();
        let user_root = memory_service.storage().user_root();
        crate::magician_v2::agents::user_memory_bounds::archive::append_batch(
            &user_root,
            &replaced,
            archived_at,
            bounds.archive.compress_after_months,
        )
        .await
        .context("archiving the records a learning promotion replaces")?;

        let (audit_row, drained) = append_learning_promotion_audit(
            &mut knowledge,
            candidate,
            spec,
            &now,
            &superseded,
            removed_count,
            if bounds.enforcing() {
                ENFORCED_PROMOTION_AUDIT_ITEMS
            } else {
                MAX_PROMOTION_AUDIT_ITEMS
            },
        )?;
        // The new row, and every row trimmed from the main file that the
        // archive does not already hold, go to the archive before the trimmed
        // document is written. If that fails the write stops here, so the
        // trimmed rows stay in `knowledge.json`.
        let already = {
            let root = user_root.clone();
            tokio::task::spawn_blocking(move || {
                crate::magician_v2::agents::user_memory_bounds::compact::archived_promotion_ids(
                    &root,
                )
            })
            .await
            .context("reading the archived promotion audit")?
        };
        let mut audit_lines: Vec<Value> = drained
            .into_iter()
            .filter(|row| {
                !already.contains(
                    &crate::magician_v2::agents::user_memory_bounds::compact::promotion_identity(
                        row,
                    ),
                )
            })
            .collect();
        audit_lines.push(audit_row);
        crate::magician_v2::agents::user_memory_bounds::archive::append_values(
            &user_root,
            crate::magician_v2::agents::user_memory_bounds::archive::LEARNING_PROMOTIONS_FILE,
            &audit_lines,
        )
        .await
        .context("archiving the learning promotion audit; nothing written")?;
        memory_service
            .persist_user_knowledge_archived_as(&knowledge, "learning", &replaced)
            .await?;

        Ok(UserMemoryWriteOutcome {
            target: format!("user.{}", spec.target_tier),
            superseded_count: superseded.len(),
            removed_count,
        })
    }
}

impl MemoryPromotionSpec {
    fn from_candidate(candidate: &LearningCandidate) -> Result<Self> {
        let payload = candidate
            .proposed_change
            .get("memory")
            .and_then(Value::as_object)
            .map(|object| Value::Object(object.clone()))
            .unwrap_or_else(|| candidate.proposed_change.clone());

        let target_scope = read_string_any(&payload, &["scope", "memory_scope", "target_scope"])
            .map(|scope| normalize_memory_token(&scope))
            .unwrap_or_default();

        let target_tier = read_string_any(&payload, &["target_tier", "tier_name", "tier"])
            .or_else(|| tier_from_target(candidate.promotion_target.as_deref()))
            .or_else(|| tier_from_target(candidate.proposed_target.as_deref()))
            .unwrap_or_else(|| default_user_tier(&candidate.candidate_type).to_string());
        let target_tier = normalize_memory_token(&target_tier);

        let operation = read_string_any(&payload, &["operation", "action"])
            .as_deref()
            .map(parse_operation)
            .unwrap_or(MemoryOperation::Upsert);

        let key = read_string_any(
            &payload,
            &[
                "key",
                "memory_key",
                "preference_key",
                "fact_key",
                "procedure_key",
                "name",
            ],
        )
        .unwrap_or_else(|| candidate.title.clone());
        let key = normalize_memory_key(&key);
        if key.is_empty() {
            return Err(anyhow!("memory payload is missing a stable key"));
        }

        let value = read_value_any(
            &payload,
            &["value", "memory_value", "preference", "fact", "procedure"],
        )
        .unwrap_or_else(|| Value::String(candidate.summary.clone()));

        let explicit_user_request = read_bool_any(
            &payload,
            &[
                "explicit_user_request",
                "explicit_request",
                "user_requested",
            ],
        );
        let explicit_user_correction = read_bool_any(
            &payload,
            &[
                "explicit_user_correction",
                "explicit_correction",
                "user_correction",
            ],
        );
        let target_scope =
            if target_scope.is_empty() && (explicit_user_request || explicit_user_correction) {
                "user".to_string()
            } else if target_scope.is_empty() {
                default_memory_scope(&candidate.candidate_type).to_string()
            } else {
                target_scope
            };
        let replaces_key = read_string_any(
            &payload,
            &[
                "replaces_key",
                "replace_key",
                "supersedes_key",
                "conflicts_with_key",
            ],
        )
        .map(|key| normalize_memory_key(&key))
        .filter(|key| !key.is_empty());

        Ok(Self {
            target_scope,
            target_tier,
            operation,
            key,
            value,
            explicit_user_request,
            explicit_user_correction,
            replaces_key,
        })
    }
}

impl MemoryOperation {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Replace => "replace",
            Self::Remove => "remove",
        }
    }
}

fn auto_promotion_blocker(
    candidate: &LearningCandidate,
    spec: &MemoryPromotionSpec,
) -> Option<String> {
    if spec.target_scope != "user" {
        return Some(format!(
            "target scope `{}` is not user memory; agent/agent-goal memory requires review",
            spec.target_scope
        ));
    }
    if !crate::magician_v2::chat::service::is_curated_user_memory_tier(&spec.target_tier) {
        return Some(format!(
            "target user tier `{}` is not in the allowlist",
            spec.target_tier
        ));
    }
    if candidate.review_required {
        return Some("reflection marked the candidate as review_required".to_string());
    }
    if !matches!(candidate.risk_level, LearningRiskLevel::Low) {
        return Some(format!(
            "risk level `{}` requires review",
            candidate.risk_level.as_str()
        ));
    }
    if !(spec.explicit_user_request || spec.explicit_user_correction) {
        return Some(
            "only explicit user memory requests/corrections are auto-promoted".to_string(),
        );
    }
    if candidate.confidence.unwrap_or_default() < MIN_EXPLICIT_MEMORY_CONFIDENCE {
        return Some(format!(
            "confidence below {:.2} auto-promotion threshold",
            MIN_EXPLICIT_MEMORY_CONFIDENCE
        ));
    }
    if contains_secretish_value(&spec.value)
        || contains_secretish_text(&spec.key)
        || spec
            .replaces_key
            .as_deref()
            .is_some_and(contains_secretish_text)
    {
        return Some("candidate may contain secret-like material".to_string());
    }
    None
}

fn default_memory_scope(candidate_type: &LearningCandidateType) -> &'static str {
    match candidate_type {
        LearningCandidateType::MemoryPreference => "user",
        LearningCandidateType::MemoryFact => "agent",
        _ => "agent",
    }
}

fn default_user_tier(candidate_type: &LearningCandidateType) -> &'static str {
    match candidate_type {
        LearningCandidateType::MemoryPreference => "preferences",
        LearningCandidateType::MemoryFact => "knowledge",
        _ => "knowledge",
    }
}

fn parse_operation(raw: &str) -> MemoryOperation {
    match normalize_memory_token(raw).as_str() {
        "remove" | "delete" | "forget" => MemoryOperation::Remove,
        "replace" | "supersede" | "correct" => MemoryOperation::Replace,
        _ => MemoryOperation::Upsert,
    }
}

fn tier_from_target(target: Option<&str>) -> Option<String> {
    let target = target?.trim();
    let stripped = target
        .strip_prefix("user.")
        .or_else(|| target.strip_prefix("memory.users."))
        .unwrap_or(target);
    let tier = stripped.split('.').next()?.trim();
    (!tier.is_empty()).then(|| tier.to_string())
}

fn read_string_any(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(|entry| match entry {
            Value::String(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        })
}

fn read_bool_any(value: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(|entry| match entry {
            Value::Bool(flag) => Some(*flag),
            Value::String(text) => match normalize_memory_token(text).as_str() {
                "true" | "yes" | "explicit" => Some(true),
                "false" | "no" => Some(false),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or(false)
}

fn read_value_any(value: &Value, keys: &[&str]) -> Option<Value> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find(|entry| !entry.is_null())
        .cloned()
}

fn normalize_memory_token(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace('-', "_")
}

fn normalize_memory_key(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn contains_secretish_value(value: &Value) -> bool {
    match value {
        Value::String(text) => contains_secretish_text(text),
        Value::Array(items) => items.iter().any(contains_secretish_value),
        Value::Object(map) => map
            .iter()
            .any(|(key, value)| contains_secretish_text(key) || contains_secretish_value(value)),
        _ => false,
    }
}

fn contains_secretish_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "api_key",
        "apikey",
        "secret",
        "token",
        "cookie",
        "authorization",
        "bearer ",
        "private key",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn memory_write_evidence(
    write: &UserMemoryWriteOutcome,
    spec: &MemoryPromotionSpec,
) -> LearningEvidenceRef {
    LearningEvidenceRef {
        kind: "memory_write".to_string(),
        id: Some(spec.key.clone()),
        path: Some(format!(
            "{USER_MEMORY_KNOWLEDGE_RELATIVE_PATH}#/{}",
            write.target.trim_start_matches("user.")
        )),
        uri: None,
        summary: Some(format!(
            "{} `{}` in {}",
            spec.operation.as_str(),
            spec.key,
            write.target
        )),
    }
}

fn append_learning_promotion_audit(
    knowledge: &mut Value,
    candidate: &LearningCandidate,
    spec: &MemoryPromotionSpec,
    now: &str,
    superseded: &[Value],
    removed_count: usize,
    keep: usize,
) -> Result<(Value, Vec<Value>)> {
    let root = knowledge
        .as_object_mut()
        .ok_or_else(|| anyhow!("user knowledge root was not an object"))?;
    let meta = root
        .entry("_meta".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !meta.is_object() {
        *meta = Value::Object(Map::new());
    }
    let meta = meta
        .as_object_mut()
        .ok_or_else(|| anyhow!("user knowledge _meta was not an object"))?;
    let promotions = meta
        .entry("learning_promotions".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !promotions.is_array() {
        *promotions = Value::Array(Vec::new());
    }
    let promotions = promotions
        .as_array_mut()
        .ok_or_else(|| anyhow!("user knowledge _meta.learning_promotions was not an array"))?;
    let row = json!({
        "candidate_id": candidate.id,
        "candidate_type": candidate.candidate_type.as_str(),
        "target_tier": spec.target_tier,
        "key": spec.key,
        "operation": spec.operation.as_str(),
        "promoted_at": now,
        "superseded_count": superseded.len(),
        "removed_count": removed_count,
        "superseded": superseded.iter().take(5).cloned().collect::<Vec<_>>(),
    });
    promotions.push(row.clone());
    // Returned for the caller to archive before this document is written.
    let drained = if promotions.len() > keep {
        let drop_count = promotions.len() - keep;
        promotions.drain(0..drop_count).collect()
    } else {
        Vec::new()
    };
    Ok((row, drained))
}

fn emit_memory_route_row(
    memory_service: &AgentMemoryService,
    candidate: &LearningCandidate,
    event_kind: &str,
    status: &str,
    target: Option<&str>,
    spec: Option<&MemoryPromotionSpec>,
    payload: Value,
) {
    let mut row = MemoryAnalyticsRow::now(event_kind, LEARNING_MEMORY_BRIDGE_SOURCE);
    row.agent_id = candidate.source_agent_id.clone();
    row.item_key = Some(candidate.id.clone());
    row.scope = spec.map(|spec| spec.target_scope.clone());
    row.tier_name = spec.map(|spec| spec.target_tier.clone()).or_else(|| {
        target
            .and_then(|value| value.split('.').nth(1))
            .map(str::to_string)
    });
    row.confidence = candidate.confidence;
    row.target = target.map(str::to_string);
    row.source_kind = Some(candidate.candidate_type.as_str().to_string());
    row.status = status.to_string();
    row.payload_json = json_payload(&json!({
        "candidate_id": candidate.id,
        "candidate_type": candidate.candidate_type.as_str(),
        "risk_level": candidate.risk_level.as_str(),
        "review_required": candidate.review_required,
        "payload": payload,
    }));
    emit_rows_for_storage(memory_service.storage(), vec![row]);
}

pub fn log_memory_route_error(candidate_id: &str, error: &anyhow::Error) {
    warn!(
        candidate_id = %candidate_id,
        error = %error,
        "Learning memory candidate routing failed"
    );
}

#[cfg(test)]
mod bounds_tests {
    use super::*;
    use crate::magician_v2::agents::user_memory_bounds::{
        archive, with_settings, MagicianMemoryBoundsSettings, UserKnowledgeBoundsMode,
    };
    use crate::magician_v2::agents::AgentMemoryResolver;

    fn candidate() -> LearningCandidate {
        let now = Utc::now();
        LearningCandidate {
            id: "cand-standup".into(),
            scope: LearningScope::new("p", "w"),
            candidate_type: LearningCandidateType::MemoryProcedure,
            state: LearningCandidateState::Approved,
            title: "Standup notes".into(),
            summary: "Post notes after standup".into(),
            rationale: "Seen across several runs".into(),
            proposed_change: json!({}),
            proposed_target: None,
            confidence: Some(0.8),
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            event_refs: Vec::new(),
            evidence_refs: Vec::new(),
            risk_level: LearningRiskLevel::Low,
            review_required: false,
            review_reason: None,
            review_policy: json!({}),
            promotion_target: None,
            promotion_policy: json!({}),
            created_at: now,
            updated_at: now,
        }
    }

    fn spec() -> MemoryPromotionSpec {
        MemoryPromotionSpec {
            target_scope: "user".into(),
            target_tier: "workflows".into(),
            operation: MemoryOperation::Upsert,
            key: "standup-notes".into(),
            value: json!("Post notes in the team channel after standup"),
            explicit_user_request: false,
            explicit_user_correction: false,
            replaces_key: None,
        }
    }

    /// Audit rows trimmed from the main file reach the archive first, once.
    #[tokio::test]
    async fn trimmed_promotion_audit_rows_are_archived_once() {
        let root = tempfile::tempdir().unwrap();
        let service = AgentMemoryResolver::new(root.path())
            .resolve_for_scope("p", "w")
            .unwrap();
        let existing: Vec<Value> = (0..205)
            .map(|i| json!({"candidate_id":format!("old-{i}"),"promoted_at":format!("2026-09-01T00:00:{:02}Z", i % 60),"seq":i}))
            .collect();
        service
            .save_user_knowledge(&json!({"_meta":{"learning_promotions": existing}}))
            .await
            .unwrap();
        let bridge = LearningMemoryBridge::new(ArtifactV2Workspace::new(root.path()));
        let mut second = spec();
        second.key = "second-key".into();
        for spec in [spec(), second] {
            bridge
                .write_user_memory(&service, &candidate(), &spec)
                .await
                .unwrap();
        }
        let document = service.load_user_knowledge().await.unwrap();
        let kept = document["_meta"]["learning_promotions"].as_array().unwrap();
        assert_eq!(kept.len(), MAX_PROMOTION_AUDIT_ITEMS);
        let audit = std::fs::read_to_string(
            archive::archive_dir(&service.storage().user_root())
                .join(archive::LEARNING_PROMOTIONS_FILE),
        )
        .unwrap();
        let rows: Vec<Value> = audit
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        // 207 rows in all: 200 kept in the file, the 7 oldest in the archive
        // (plus the two new rows, which are always archived).
        let archived_old: Vec<i64> = rows
            .iter()
            .filter_map(|row| row.get("seq").and_then(Value::as_i64))
            .collect();
        assert_eq!(archived_old, (0..7).collect::<Vec<_>>());
        assert_eq!(rows.len(), 9);
        let identities: std::collections::BTreeSet<String> = rows
            .iter()
            .map(crate::magician_v2::agents::user_memory_bounds::compact::promotion_identity)
            .collect();
        assert_eq!(identities.len(), rows.len(), "no row archived twice");
        // Every original row is either kept or archived.
        for i in 0..205_i64 {
            assert!(
                archived_old.contains(&i) || kept.iter().any(|row| row["seq"] == i),
                "row {i} lost"
            );
        }
    }

    /// A learning promotion into a 2,000-record section deletes nothing: the
    /// record it replaces is archived, the section cap archives only
    /// unprotected records, and every original is live or archived.
    #[tokio::test]
    async fn a_promotion_into_a_crowded_section_deletes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let service = AgentMemoryResolver::new(root.path())
            .resolve_for_scope("p", "w")
            .unwrap();
        let mut items: Vec<Value> = (0..1990)
            .map(|i| {
                json!({"key":format!("wf-{i}"),"value":format!("Step {i}"),
                    "source_type":"inferred","updated_at":"2026-09-01T00:00:00+00:00"})
            })
            .collect();
        items.extend((0..9).map(|i| {
            json!({"key":format!("owner-{i}"),"value":format!("Owner rule {i}"),
                "source_type":"explicit_user_statement","updated_at":"2026-08-01T00:00:00+00:00"})
        }));
        items.push(json!({"key":"standup-notes","value":"Old wording",
            "source_type":"inferred","updated_at":"2026-08-01T00:00:00+00:00"}));
        service
            .save_user_knowledge(&json!({ "workflows": items }))
            .await
            .unwrap();

        let mut enforce = MagicianMemoryBoundsSettings::default();
        enforce.mode = UserKnowledgeBoundsMode::Enforce;
        let bridge = LearningMemoryBridge::new(ArtifactV2Workspace::new(root.path()));
        with_settings(enforce, async {
            bridge
                .write_user_memory(&service, &candidate(), &spec())
                .await
                .unwrap();
        })
        .await;

        let document = service.load_user_knowledge().await.unwrap();
        let live = document["workflows"].as_array().unwrap();
        assert_eq!(live.len(), 600);
        let user_root = service.storage().user_root();
        let archived: Vec<archive::ArchiveEntry> = archive::month_files(&user_root)
            .iter()
            .flat_map(|path| archive::read_file(path).unwrap().0)
            .collect();
        // 2,000 originals + 1 new record, every one live or archived.
        assert_eq!(live.len() + archived.len(), 2001);
        assert_eq!(
            archived
                .iter()
                .filter(|e| e.reason == "learning_promotion_replaced")
                .count(),
            1
        );
        for i in 0..9 {
            let key = format!("owner-{i}");
            assert!(live.iter().any(|item| item["key"] == key.as_str()));
        }
        assert!(live
            .iter()
            .any(|item| item["value"] == "Post notes in the team channel after standup"));
        assert_eq!(
            document["_meta"]["learning_promotions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let audit = std::fs::read_to_string(
            archive::archive_dir(&user_root).join(archive::LEARNING_PROMOTIONS_FILE),
        )
        .unwrap();
        assert_eq!(audit.lines().count(), 1);
    }
}
