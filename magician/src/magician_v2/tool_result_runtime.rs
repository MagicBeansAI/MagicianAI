//! Shared post-dispatch tool-result lifecycle used by every agent surface.
//!
//! Callers provide an already-authorized, post-redaction JSON result. This
//! service atomically materializes the complete value, derives one bounded
//! structural projection, and returns the versioned transcript-ready record.

use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;

use crate::config::AgentSurfaceResultProjectionBudgetConfig;
use crate::magician_v2::artifact_v2::{service::ScopeRef, workspace::ArtifactV2Workspace};
use crate::magician_v2::tool_result_materialization::{
    CanonicalRawResultStore, CanonicalResultError, MaterializeRawResultRequest, RawResultIdentity,
    RawResultOwner, ResultAuthorityBinding, ResultRetentionClass, SnapshotResultAuthority,
};
use crate::magician_v2::tool_result_projection::{
    ConservativeTokenEstimator, DisplayResultProjection, ProjectedToolResultV1, ProjectionBudget,
    ProjectionContractSpec, ProjectionError, ToolOutcome, ToolOutcomeStatus, ToolResultIdentity,
    ToolResultProjectionRequest, ToolResultProjector,
};

#[derive(Debug, Clone)]
pub struct MaterializeAndProjectToolResultRequest<'a> {
    pub scope: ScopeRef,
    pub owner: RawResultOwner,
    pub agent_id: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub trust_tool: String,
    pub trust_action: String,
    pub execution_id: Option<String>,
    pub task_id: Option<String>,
    pub authority_revision: String,
    /// Typed execution outcome supplied by the dispatch rail. The shared
    /// runtime never guesses lifecycle state from arbitrary result fields.
    pub outcome: ToolOutcome,
    pub safe_value: &'a Value,
    pub media_type: String,
    pub retention_class: ResultRetentionClass,
    pub expires_at_ms: Option<i64>,
    /// Complete capability-owned projection policy. Passing only its id would
    /// silently discard custom record paths, priorities, and atomic groups.
    pub projection_contract: Option<&'a ProjectionContractSpec>,
    pub projection_budget: ProjectionBudget,
    pub spoken_hint: Option<&'a str>,
}

#[derive(Debug, Error)]
pub enum ToolResultRuntimeError {
    #[error("canonical result materialization failed: {0}")]
    Materialization(#[from] CanonicalResultError),
    #[error("tool-result projection failed: {0}")]
    Projection(#[from] ProjectionError),
}

pub async fn materialize_and_project_tool_result(
    workspace: ArtifactV2Workspace,
    request: MaterializeAndProjectToolResultRequest<'_>,
) -> Result<ProjectedToolResultV1, ToolResultRuntimeError> {
    // `safe_value` is the complete post-redaction canonical result. Exact
    // injected-value replacement and capability-owned content policy happen
    // in the dispatcher. Never run pattern/key heuristics here: doing so would
    // irreversibly corrupt legitimate fields such as pagination `token`, a
    // document's `secret`, or domain-level `authorization` state.
    let safe_value = request.safe_value.clone();
    let identity = RawResultIdentity {
        agent_id: request.agent_id.clone(),
        tool_name: request.tool_name.clone(),
        tool_call_id: request.tool_call_id.clone(),
        trust_tool: Some(request.trust_tool.clone()),
        trust_action: Some(request.trust_action.clone()),
    };
    let binding = ResultAuthorityBinding {
        agent_id: request.agent_id.clone(),
        owner: request.owner.clone(),
        tool_name: request.tool_name.clone(),
        tool_call_id: request.tool_call_id.clone(),
        trust_tool: Some(request.trust_tool),
        trust_action: Some(request.trust_action),
    };
    let authority = SnapshotResultAuthority::for_current_binding(
        request.scope.clone(),
        binding,
        request.authority_revision.clone(),
    )?;
    let store = CanonicalRawResultStore::new(workspace, Arc::new(authority));
    let descriptor = store
        .materialize(MaterializeRawResultRequest {
            scope: request.scope.clone(),
            owner: request.owner,
            identity,
            authority_revision: request.authority_revision.clone(),
            safe_value: safe_value.clone(),
            media_type: request.media_type,
            retention_class: request.retention_class,
            expires_at_ms: request.expires_at_ms,
        })
        .await?;
    let raw = descriptor.to_projection_descriptor();
    let display = if raw.size_bytes
        <= u64::try_from(request.projection_budget.max_serialized_bytes).unwrap_or(u64::MAX)
    {
        DisplayResultProjection::InlineAndReferenced {
            value: safe_value.clone(),
            content_ref: raw.content_ref.clone(),
            content_hash: raw.content_hash.clone(),
            media_type: raw.media_type.clone(),
            size_bytes: raw.size_bytes,
        }
    } else {
        DisplayResultProjection::referenced(&raw)
    };
    let mut registry =
        crate::magician_v2::tool_result_projection::ProjectionContractRegistry::default();
    if let Some(contract) = request.projection_contract {
        registry.register_override(contract.clone())?;
    }
    let requested_contract_id = request
        .projection_contract
        .map(|contract| contract.contract_id.clone());
    let projector = ToolResultProjector::new(registry, ConservativeTokenEstimator);
    Ok(projector.project(ToolResultProjectionRequest {
        identity: ToolResultIdentity {
            tool_name: request.tool_name,
            tool_call_id: request.tool_call_id,
            execution_id: request.execution_id,
            task_id: request.task_id,
            scope_digest: scope_digest(&request.scope),
            authority_revision: request.authority_revision,
        },
        outcome: request.outcome,
        raw_result: &safe_value,
        raw,
        display,
        spoken_hint: request.spoken_hint,
        contract_id: requested_contract_id.as_ref(),
        budget: request.projection_budget,
    })?)
}

pub fn projection_budget_from_config(
    config: &AgentSurfaceResultProjectionBudgetConfig,
) -> ProjectionBudget {
    ProjectionBudget {
        max_serialized_bytes: config.max_serialized_bytes,
        max_estimated_tokens: config.max_model_tokens,
        max_records: config.max_records,
        max_depth: config.max_depth,
        max_scalar_bytes: config.max_scalar_bytes,
        max_spoken_chars: 420,
    }
}

/// Decode only the platform's declared lifecycle vocabulary, using the typed
/// dispatch rail outcome for values whose `status` field is domain data.
///
/// This is a compatibility decoder for today's JSON-returning dispatchers; it
/// is not free-form inference. Unknown statuses never manufacture success:
/// they leave the caller-supplied typed fallback unchanged. A future typed
/// dispatcher can pass `ToolOutcome` directly and bypass this helper.
pub fn legacy_tool_outcome_from_platform_envelope(
    value: &Value,
    dispatch_fallback: ToolOutcomeStatus,
) -> ToolOutcome {
    let explicit_status = value
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .map(|status| status.to_ascii_lowercase());
    let outcome_status = match explicit_status.as_deref() {
        Some(
            "ok" | "success" | "succeeded" | "complete" | "completed" | "done" | "subscribed"
            | "already_subscribed" | "recorded" | "noop_terminal",
        ) => ToolOutcomeStatus::Succeeded,
        Some("partial" | "degraded") => ToolOutcomeStatus::Partial,
        Some("denied" | "forbidden" | "unauthorized" | "duplicate_blocked") => {
            ToolOutcomeStatus::Denied
        },
        Some(
            "error"
            | "failed"
            | "failure"
            | "rejected"
            | "blocked"
            | "not_found"
            | "aborted"
            | "ignored"
            | "not_supported"
            | "not_supported_in_chat"
            | "unavailable"
            | "record_failed",
        ) => ToolOutcomeStatus::Failed,
        Some("timeout" | "timed_out" | "expired") => ToolOutcomeStatus::TimedOut,
        Some("revoked") => ToolOutcomeStatus::Revoked,
        Some("cancelled" | "canceled") => ToolOutcomeStatus::Cancelled,
        Some(
            "pending" | "queued" | "enqueued" | "running" | "delegated" | "in_progress"
            | "processing" | "accepted" | "scheduled" | "streaming" | "executing" | "started"
            | "deferred",
        ) => ToolOutcomeStatus::Pending,
        Some("requires_approval" | "approval_required" | "needs_approval" | "pending_approval") => {
            ToolOutcomeStatus::RequiresApproval
        },
        Some(_) => dispatch_fallback,
        None if value.get("success").and_then(Value::as_bool) == Some(false) => {
            ToolOutcomeStatus::Failed
        },
        None if value.get("success").and_then(Value::as_bool) == Some(true) => {
            ToolOutcomeStatus::Succeeded
        },
        None if value.get("error").is_some_and(|error| !error.is_null())
            || value
                .get("error_code")
                .is_some_and(|error_code| !error_code.is_null()) =>
        {
            ToolOutcomeStatus::Failed
        },
        None => dispatch_fallback,
    };
    let code = value
        .get("error_code")
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let message = value
        .get("reason")
        .or_else(|| value.get("message"))
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let retryable = value
        .get("retryable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ToolOutcome {
        status: outcome_status,
        code,
        message,
        retryable,
    }
}

/// Provider-balanced fail-closed value used only when canonical storage or
/// projection cannot be completed. It preserves the real outcome class and
/// small complete diagnostics, but never falls back to a character prefix of
/// the raw result and never advertises a continuation that does not exist.
pub fn balanced_projection_failure_value(value: &Value, failure_class: &str) -> Value {
    // This helper is deliberately safe to call from an error path that failed
    // before canonical materialization. Never assume its caller reached the
    // normal pre-persistence sanitizer: otherwise an echoed credential in an
    // error message could be persisted in a legacy transcript or sent to the
    // provider precisely when the primary safety boundary is unavailable.
    let safe_value = crate::magician_v2::secrets::sanitize_json_for_provider(value);
    let safe_failure_class = crate::magician_v2::secrets::sanitize_text_for_provider(failure_class);
    let outcome =
        legacy_tool_outcome_from_platform_envelope(&safe_value, ToolOutcomeStatus::Unknown);
    let status = match outcome.status {
        ToolOutcomeStatus::Succeeded => "ok",
        ToolOutcomeStatus::Partial => "partial",
        ToolOutcomeStatus::Failed => "error",
        ToolOutcomeStatus::Denied => "denied",
        ToolOutcomeStatus::Cancelled => "cancelled",
        ToolOutcomeStatus::Pending => "pending",
        ToolOutcomeStatus::RequiresApproval => "requires_approval",
        ToolOutcomeStatus::TimedOut => "timed_out",
        ToolOutcomeStatus::Revoked => "revoked",
        ToolOutcomeStatus::Unknown => "unknown",
    };
    let bounded_message = outcome
        .message
        .and_then(|message| (message.len() <= 1_024).then_some(message));
    serde_json::json!({
        "status": status,
        "error_code": outcome.code,
        "message": bounded_message,
        "retryable": outcome.retryable,
        "projection": {
            "available": false,
            "failure_class": safe_failure_class,
            "raw_result_included": false,
            "continuation_available": false
        }
    })
}

fn scope_digest(scope: &ScopeRef) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.tool-result.scope.v1\0");
    hasher.update(scope.principal().as_bytes());
    hasher.update(b"\0");
    hasher.update(scope.workspace().as_bytes());
    hasher.finalize().to_hex().to_string()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn queued_receipts_never_become_success() {
        let outcome = legacy_tool_outcome_from_platform_envelope(
            &serde_json::json!({"status": "enqueued", "task_id": "task-1"}),
            ToolOutcomeStatus::Unknown,
        );
        assert_eq!(outcome.status, ToolOutcomeStatus::Pending);
    }

    #[test]
    fn action_result_success_flag_preserves_failure_without_status_field() {
        let outcome = legacy_tool_outcome_from_platform_envelope(
            &serde_json::json!({"success": false, "error": "browser action failed"}),
            ToolOutcomeStatus::Unknown,
        );
        assert_eq!(outcome.status, ToolOutcomeStatus::Failed);
        assert_eq!(outcome.message.as_deref(), Some("browser action failed"));
    }

    #[test]
    fn explicit_status_takes_precedence_over_legacy_success_flag() {
        let outcome = legacy_tool_outcome_from_platform_envelope(
            &serde_json::json!({"status": "pending", "success": false}),
            ToolOutcomeStatus::Unknown,
        );
        assert_eq!(outcome.status, ToolOutcomeStatus::Pending);
    }

    #[test]
    fn explicit_denial_timeout_and_unsupported_statuses_never_become_success() {
        for (status, expected) in [
            ("denied", ToolOutcomeStatus::Denied),
            ("forbidden", ToolOutcomeStatus::Denied),
            ("timeout", ToolOutcomeStatus::TimedOut),
            ("timed_out", ToolOutcomeStatus::TimedOut),
            ("not_found", ToolOutcomeStatus::Failed),
            ("not_supported_in_chat", ToolOutcomeStatus::Failed),
        ] {
            assert_eq!(
                legacy_tool_outcome_from_platform_envelope(
                    &serde_json::json!({"status": status}),
                    ToolOutcomeStatus::Unknown,
                )
                .status,
                expected,
                "status {status}"
            );
        }
    }

    #[test]
    fn active_work_statuses_remain_pending() {
        for status in ["in_progress", "processing", "accepted", "streaming"] {
            assert_eq!(
                legacy_tool_outcome_from_platform_envelope(
                    &serde_json::json!({"status": status}),
                    ToolOutcomeStatus::Unknown,
                )
                .status,
                ToolOutcomeStatus::Pending,
                "status {status}"
            );
        }
    }

    #[test]
    fn null_error_fields_do_not_override_typed_dispatch_success() {
        let outcome = legacy_tool_outcome_from_platform_envelope(
            &serde_json::json!({"value": 27, "error": null, "error_code": null}),
            ToolOutcomeStatus::Succeeded,
        );
        assert_eq!(outcome.status, ToolOutcomeStatus::Succeeded);
    }

    #[test]
    fn unknown_domain_status_never_manufactures_success() {
        let unknown = legacy_tool_outcome_from_platform_envelope(
            &serde_json::json!({"status": "mystery-domain-state"}),
            ToolOutcomeStatus::Unknown,
        );
        assert_eq!(unknown.status, ToolOutcomeStatus::Unknown);

        let typed_success = legacy_tool_outcome_from_platform_envelope(
            &serde_json::json!({"status": "created"}),
            ToolOutcomeStatus::Succeeded,
        );
        assert_eq!(typed_success.status, ToolOutcomeStatus::Succeeded);
    }

    #[test]
    fn partial_and_revoked_are_preserved_as_distinct_outcomes() {
        assert_eq!(
            legacy_tool_outcome_from_platform_envelope(
                &serde_json::json!({"status": "partial"}),
                ToolOutcomeStatus::Unknown,
            )
            .status,
            ToolOutcomeStatus::Partial
        );
        assert_eq!(
            legacy_tool_outcome_from_platform_envelope(
                &serde_json::json!({"status": "revoked"}),
                ToolOutcomeStatus::Unknown,
            )
            .status,
            ToolOutcomeStatus::Revoked
        );
    }

    #[test]
    fn config_budget_maps_without_surface_specific_semantics() {
        let config = AgentSurfaceResultProjectionBudgetConfig {
            max_model_tokens: 2_048,
            max_serialized_bytes: 8_192,
            max_records: 10,
            max_depth: 8,
            max_scalar_bytes: 4_096,
        };
        let budget = projection_budget_from_config(&config);
        assert_eq!(budget.max_estimated_tokens, 2_048);
        assert_eq!(budget.max_serialized_bytes, 8_192);
        assert_eq!(budget.max_records, 10);
    }

    #[test]
    fn balanced_projection_failure_is_safe_before_any_other_boundary_guard() {
        let value = serde_json::json!({
            "status": "error",
            "error_code": "provider_error",
            "message": "Authorization: Bearer escaped-secret-token",
            "authorization": "Bearer another-escaped-secret"
        });

        let balanced =
            balanced_projection_failure_value(&value, "materialization_or_projection_failed");

        assert_eq!(balanced["status"], "error");
        assert_eq!(balanced["error_code"], "provider_error");
        assert!(!balanced.to_string().contains("escaped-secret"));
        assert_eq!(
            balanced["projection"]["failure_class"],
            "materialization_or_projection_failed"
        );
    }

    #[tokio::test]
    async fn canonical_persistence_preserves_ambiguous_domain_fields() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope =
            ScopeRef::system_internal_unauthenticated(&"owner".to_string(), &"default".to_string());
        let owner = RawResultOwner::Chat {
            session_id: "chat-safe".to_string(),
        };
        let now = chrono::Utc::now().timestamp_millis();
        let session_document = crate::magician_v2::chat::models::ChatSessionDocument {
            format_version: 2,
            session: crate::magician_v2::chat::models::ChatSession {
                internal_voice: None,
                id: "chat-safe".to_string(),
                principal: scope.principal().to_string(),
                workspace: scope.workspace().to_string(),
                agent_id: "personal-assistant".to_string(),
                ui_thread_id: "tool-result-runtime-test".to_string(),
                title: None,
                origin_channel: crate::magician_v2::chat::models::ChatChannel::web(),
                status: crate::magician_v2::chat::models::ChatSessionStatus::Active,
                history_lane: crate::magician_v2::history::HistoryLane::Personal,
                is_default_session: false,
                created_at: now,
                updated_at: now,
            },
            messages: Vec::new(),
            llm_history: Vec::new(),
        };
        workspace
            .ensure_chat_session_workspace(&scope.principal(), &scope.workspace(), "chat-safe")
            .await
            .unwrap();
        workspace
            .write_json_atomic_path(
                workspace.chat_session_path(&scope.principal(), &scope.workspace(), "chat-safe"),
                &session_document,
            )
            .await
            .unwrap();
        let projection = materialize_and_project_tool_result(
            workspace.clone(),
            MaterializeAndProjectToolResultRequest {
                scope: scope.clone(),
                owner: owner.clone(),
                agent_id: "personal-assistant".to_string(),
                tool_name: "fixture_tool".to_string(),
                tool_call_id: "safe-call".to_string(),
                trust_tool: "fixture_tool".to_string(),
                trust_action: "execute".to_string(),
                execution_id: None,
                task_id: None,
                authority_revision: "authority-safe".to_string(),
                outcome: ToolOutcome::succeeded(),
                safe_value: &serde_json::json!({
                    "status": "ok",
                    "relationship": "wife",
                    "value": "14 September",
                    "token": "pagination-cursor-27",
                    "secret": "surprise party",
                    "cookie": "chocolate chip",
                    "authorization": "domain approval granted"
                }),
                media_type: "application/json".to_string(),
                retention_class: ResultRetentionClass::ChatLifecycle,
                expires_at_ms: None,
                projection_contract: None,
                projection_budget: ProjectionBudget::default(),
                spoken_hint: Some("The domain token is pagination-cursor-27."),
            },
        )
        .await
        .unwrap();

        let encoded_projection = serde_json::to_string(&projection).unwrap();
        assert!(encoded_projection.contains("pagination-cursor-27"));
        assert!(encoded_projection.contains("surprise party"));
        assert!(encoded_projection.contains("chocolate chip"));
        assert!(encoded_projection.contains("domain approval granted"));
        assert!(encoded_projection.contains("14 September"));

        let binding = ResultAuthorityBinding {
            agent_id: "personal-assistant".to_string(),
            owner: owner.clone(),
            tool_name: "fixture_tool".to_string(),
            tool_call_id: "safe-call".to_string(),
            trust_tool: Some("fixture_tool".to_string()),
            trust_action: Some("execute".to_string()),
        };
        let authority =
            SnapshotResultAuthority::for_current_binding(scope.clone(), binding, "authority-safe")
                .unwrap();
        let page = CanonicalRawResultStore::new(workspace, Arc::new(authority))
            .read(
                &crate::magician_v2::tool_result_materialization::RawResultReadContext {
                    scope,
                    owner,
                    agent_id: "personal-assistant".to_string(),
                },
                &crate::magician_v2::tool_result_materialization::RawResultReadRequest::first_page(
                    crate::magician_v2::tool_result_materialization::ScopedResultRef::parse(
                        projection.raw.content_ref.result_ref.clone(),
                    )
                    .unwrap(),
                    20,
                ),
            )
            .await
            .unwrap();
        let persisted = serde_json::to_string(&page.entries).unwrap();
        assert!(persisted.contains("pagination-cursor-27"));
        assert!(persisted.contains("surprise party"));
        assert!(persisted.contains("chocolate chip"));
        assert!(persisted.contains("domain approval granted"));
        assert!(persisted.contains("14 September"));
    }
}
