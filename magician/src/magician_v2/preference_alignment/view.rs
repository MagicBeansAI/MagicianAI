//! The owner's view of their preferences, exactly as checks see them.
//!
//! [`build_view`] runs the same `collect_inventory` a check runs and applies
//! the owner's policy the same way, so the view and the checks cannot
//! disagree. It adds what the checks leave out and why: retired records
//! (superseded, retracted, out of window) and records that are not safely
//! readable, with a reason code; plus when a check last judged each one.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use super::admission::alignment_log_path;
use super::intention::{
    Coverage, EvidenceRole, Intention, IntentionSource, StandingGeneration,
};
use super::inventory::{collect_inventory_for_memory, InventoryRequest};
use super::policy::read_policy;
use super::sources::native::{
    bears_intention, eligibility, intention_id, render_statement, Eligibility,
};
use super::sources::taste_profile::DEFAULT_TASTE_PROFILE_ADMISSION_TIMEOUT;
use crate::magician_v2::agents::memory_lifecycle::digest;
use crate::magician_v2::agents::AgentMemoryResolver;

pub const PREFERENCE_VIEW_SCHEMA_VERSION: &str = "preference_view.v1";

/// Where an intention's force comes from, for the owner to read.
#[derive(Debug, Clone, Serialize)]
pub struct Ground {
    pub role: EvidenceRole,
    pub source_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntentionRow {
    #[serde(flatten)]
    pub intention: Intention,
    pub grounds: Vec<Ground>,
    /// Only the owner's native records are edited here; profile- and
    /// app-sourced intentions are changed where they live.
    pub editable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_checked_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetiredRow {
    pub id: String,
    pub tier: String,
    pub statement: String,
    /// `superseded`, `retracted`, `expired`, ... or `out_of_window`.
    pub lifecycle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    pub revision: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OmittedRow {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// `unknown_lifecycle`, `app_governed`, `unreadable_validity`,
    /// `duplicate_id`, `unreadable_tier` or `unreadable`.
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreferenceView {
    pub schema_version: &'static str,
    pub intentions: Vec<IntentionRow>,
    pub retired: Vec<RetiredRow>,
    pub omitted: Vec<OmittedRow>,
    pub coverage: Coverage,
    pub generation: StandingGeneration,
    /// The owner policy revision the per-intention policies come from;
    /// `None` when the policy is unreadable (checks are then unresolved).
    pub policy_revision: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    #[error("owner scope could not be resolved: {0}")]
    Scope(String),
}

/// When each intention was last judged, from the check log's `judged` lists.
async fn last_checked(log: &std::path::Path) -> BTreeMap<String, i64> {
    let mut latest = BTreeMap::new();
    for path in [log.with_extension("jsonl.1"), log.to_path_buf()] {
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        for line in text.lines() {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let (Some(judged), Some(at)) = (
                entry.get("judged").and_then(Value::as_array),
                entry.get("at_ms").and_then(Value::as_i64),
            ) else {
                continue;
            };
            for id in judged.iter().filter_map(|j| j.get("id").and_then(Value::as_str)) {
                let slot = latest.entry(id.to_string()).or_insert(at);
                *slot = (*slot).max(at);
            }
        }
    }
    latest
}

fn retired_and_omitted(
    knowledge: &Value,
    now: DateTime<Utc>,
) -> (Vec<RetiredRow>, BTreeMap<String, (String, &'static str)>) {
    let mut retired = Vec::new();
    let mut omitted = BTreeMap::new();
    let Some(tiers) = knowledge.as_object() else {
        return (retired, omitted);
    };
    for (tier, items) in tiers {
        let Some(items) = items.as_array() else {
            continue;
        };
        for item in items.iter().filter(|item| bears_intention(tier, item)) {
            let id = intention_id(tier, item);
            match eligibility(item, now) {
                Eligibility::Eligible { .. } => {},
                Eligibility::Retired | Eligibility::OutOfWindow => retired.push(RetiredRow {
                    id,
                    tier: tier.clone(),
                    statement: render_statement(item),
                    lifecycle: match eligibility(item, now) {
                        Eligibility::OutOfWindow => "out_of_window".to_string(),
                        _ => item
                            .get("memory_lifecycle")
                            .and_then(Value::as_str)
                            .unwrap_or("retired")
                            .to_string(),
                    },
                    superseded_by: item
                        .get("superseded_by")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    revision: digest(item),
                }),
                Eligibility::Omitted(reason) => {
                    omitted.insert(id, (tier.clone(), reason));
                },
            }
        }
    }
    (retired, omitted)
}

/// Build the owner's view for `(principal, workspace)`.
pub async fn build_view(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
) -> Result<PreferenceView, ViewError> {
    let memory = resolver
        .resolve_for_scope(principal, workspace)
        .map_err(|error| ViewError::Scope(error.to_string()))?;
    let now = Utc::now();
    let taste_loader = crate::magician_v2::taste_profile::global_taste_profile_loader();
    let taste_capture = crate::magician_v2::taste_capture::global_capture_service();
    let mut inventory = collect_inventory_for_memory(
        &memory,
        InventoryRequest {
            principal,
            workspace,
            taste_loader: taste_loader.as_deref(),
            taste_capture: taste_capture.as_deref(),
            taste_timeout: DEFAULT_TASTE_PROFILE_ADMISSION_TIMEOUT,
            now,
        },
    )
    .await;
    let policy_revision = match read_policy(resolver, principal, workspace).await {
        Ok(policy) => {
            policy.apply(&mut inventory.intentions);
            Some(policy.revision)
        },
        Err(_) => None,
    };
    let knowledge = memory.load_user_knowledge().await.unwrap_or(Value::Null);
    let (retired, native_omitted) = retired_and_omitted(&knowledge, now);
    let checked = last_checked(&alignment_log_path(memory.storage())).await;

    let omitted = inventory
        .coverage
        .omitted_ids
        .iter()
        .map(|id| match native_omitted.get(id) {
            Some((tier, reason)) => OmittedRow {
                id: id.clone(),
                tier: Some(tier.clone()),
                reason: (*reason).to_string(),
            },
            None => OmittedRow {
                id: id.clone(),
                tier: id.strip_prefix("tier:").map(str::to_owned),
                reason: if id.starts_with("tier:") {
                    "unreadable_tier"
                } else if inventory.intentions.iter().any(|i| &i.id == id) {
                    "duplicate_id"
                } else {
                    "unreadable"
                }
                .to_string(),
            },
        })
        .collect();

    let intentions = inventory
        .intentions
        .into_iter()
        .map(|intention| IntentionRow {
            grounds: intention
                .evidence_refs
                .iter()
                .map(|evidence| Ground {
                    role: evidence.role,
                    source_id: evidence.source_id.clone(),
                })
                .collect(),
            editable: matches!(intention.source, IntentionSource::NativeTier { .. }),
            last_checked_at_ms: checked.get(&intention.id).copied(),
            intention,
        })
        .collect();

    Ok(PreferenceView {
        schema_version: PREFERENCE_VIEW_SCHEMA_VERSION,
        intentions,
        retired,
        omitted,
        coverage: inventory.coverage,
        generation: inventory.generation,
        policy_revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::preference_alignment::intention::AssertionStatus;
    use serde_json::json;

    #[tokio::test]
    async fn the_view_lists_what_checks_see_plus_what_they_skip() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let memory = resolver.resolve_for_scope("owner", "default").unwrap();
        memory
            .save_user_knowledge(&json!({"preferences": [
                {"memory_record_id": "x", "key": "dinner", "value": "No meat",
                 "source_type": "explicit_user_statement",
                 "memory_evidence": [{"source_role": "user", "source_id": "turn-1", "quote": "No meat"}]},
                {"memory_record_id": "y", "key": "coffee", "value": "decaf after 3pm"},
                {"memory_record_id": "z", "key": "old", "value": "spicy",
                 "memory_lifecycle": "superseded", "superseded_by": "x"},
                {"memory_record_id": "w", "key": "bad", "value": "v", "valid_until": "not a date"},
                {"memory_record_id": "f", "key": "fact", "value": "lives in Leeds", "memory_kind": "fact"}
            ]}))
            .await
            .unwrap();
        let view = build_view(&resolver, "owner", "default").await.unwrap();
        let ids: Vec<&str> = view.intentions.iter().map(|row| row.intention.id.as_str()).collect();
        assert_eq!(ids, ["x", "y"]);
        assert_eq!(
            view.intentions[0].intention.assertion_status,
            AssertionStatus::DirectOwnerStatement
        );
        assert!(view.intentions.iter().all(|row| row.editable));
        assert!(view
            .omitted
            .iter()
            .any(|row| row.id == "w" && row.reason == "unreadable_validity"));
        assert!(view
            .retired
            .iter()
            .any(|row| row.id == "z" && row.superseded_by.as_deref() == Some("x")));
        assert!(view.retired.iter().all(|row| row.id != "f"));
        assert_eq!(view.policy_revision, Some(0));
    }
}
