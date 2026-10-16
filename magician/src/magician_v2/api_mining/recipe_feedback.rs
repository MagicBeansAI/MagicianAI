//! Shared feedback sink for task-recipe replay steps.
//!
//! Every replay surface resolves the same per-scope sink. This keeps capability
//! counter updates serialized across concurrently-running recipes and routes
//! successful JSON responses into the same projection pipeline used by the
//! original single-capability replay path.

use super::projection_pipeline::{pipeline_for_scope, IngestOutcome, ProjectionPipelineState};
use super::registry::CapabilityRegistry;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::database_owners::{database_file_path, DatabaseOwner};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

struct ScopeFeedbackState {
    registry: Mutex<CapabilityRegistry>,
    projection: Arc<ProjectionPipelineState>,
}

/// Cheap, cloneable handle used by one or more recipe runners.
#[derive(Clone)]
pub struct RecipeFeedbackSink {
    state: Arc<ScopeFeedbackState>,
}

static SCOPED_FEEDBACK: OnceLock<
    RwLock<HashMap<(String, String, PathBuf), Arc<ScopeFeedbackState>>>,
> = OnceLock::new();

fn feedback_registry(
) -> &'static RwLock<HashMap<(String, String, PathBuf), Arc<ScopeFeedbackState>>> {
    SCOPED_FEEDBACK.get_or_init(|| RwLock::new(HashMap::new()))
}

impl RecipeFeedbackSink {
    /// Resolve the process-wide sink for this exact scope and storage root.
    pub fn for_scope(
        workspace_layout: &ArtifactV2Workspace,
        principal: &str,
        workspace: &str,
    ) -> Result<Self, String> {
        let mining_base = workspace_layout.api_mining_root(principal, workspace);
        let key = (
            principal.to_owned(),
            workspace.to_owned(),
            mining_base.clone(),
        );
        {
            let read = feedback_registry()
                .read()
                .map_err(|_| "recipe feedback registry RwLock poisoned".to_string())?;
            if let Some(state) = read.get(&key) {
                return Ok(Self {
                    state: Arc::clone(state),
                });
            }
        }

        let mut write = feedback_registry()
            .write()
            .map_err(|_| "recipe feedback registry RwLock poisoned".to_string())?;
        if let Some(state) = write.get(&key) {
            return Ok(Self {
                state: Arc::clone(state),
            });
        }

        let projection_db = database_file_path(
            workspace_layout,
            principal,
            workspace,
            DatabaseOwner::ApiMining,
        );
        let projection_records = projection_db.parent().ok_or_else(|| {
            "api mining projection database path has no parent directory".to_string()
        })?;
        let state = Arc::new(ScopeFeedbackState {
            registry: Mutex::new(CapabilityRegistry::with_base_path(&mining_base)?),
            projection: pipeline_for_scope(principal, workspace, projection_records)?,
        });
        write.insert(key, Arc::clone(&state));
        Ok(Self { state })
    }

    /// Drop the cached registry and projection handles for a destructively
    /// purged scope. A later re-enable must rebuild both from the now-empty
    /// durable store rather than reuse SQLite/index state opened before purge.
    pub fn forget_scope(
        workspace_layout: &ArtifactV2Workspace,
        principal: &str,
        workspace: &str,
    ) -> bool {
        let key = (
            principal.to_owned(),
            workspace.to_owned(),
            workspace_layout.api_mining_root(principal, workspace),
        );
        feedback_registry()
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&key)
            .is_some()
    }

    /// Record one HTTP step. Auth expiry is observability-only and never
    /// demotes the underlying capability. Projection failures are isolated
    /// from the successful replay result and surfaced through structured logs.
    pub fn record(
        &self,
        origin: &str,
        capability_id: &str,
        url_template: &str,
        success: bool,
        auth_stale: bool,
        status: u16,
        response_body: &str,
    ) {
        self.record_capability(origin, capability_id, success, auth_stale, status);
        if !success {
            return;
        }
        let response = match serde_json::from_str::<serde_json::Value>(response_body) {
            Ok(response) => response,
            Err(error) => {
                tracing::debug!(
                    target: "magician::api_mining",
                    origin,
                    capability_id,
                    status,
                    %error,
                    "api_mining.recipe_feedback.projection_skipped_non_json"
                );
                return;
            },
        };
        let outcome = self
            .state
            .projection
            .ingest_response(capability_id, url_template, &response);
        log_projection_outcome(origin, capability_id, status, outcome);
    }

    fn record_capability(
        &self,
        origin: &str,
        capability_id: &str,
        success: bool,
        auth_stale: bool,
        status: u16,
    ) {
        let mut registry = match self.state.registry.lock() {
            Ok(registry) => registry,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut capability = match registry.get_capability(origin, capability_id) {
            Ok(capability) => capability,
            Err(error) => {
                tracing::warn!(
                    target: "magician::api_mining",
                    origin,
                    capability_id,
                    status,
                    %error,
                    "api_mining.recipe_feedback.capability_load_failed"
                );
                return;
            },
        };
        if auth_stale {
            capability.record_auth_failure();
        } else if success {
            capability.record_replay_success();
        } else {
            capability.record_replay_failure();
        }
        if let Err(error) = registry.register(&capability) {
            tracing::warn!(
                target: "magician::api_mining",
                origin,
                capability_id,
                status,
                %error,
                "api_mining.recipe_feedback.capability_save_failed"
            );
        }
    }
}

fn log_projection_outcome(origin: &str, capability_id: &str, status: u16, outcome: IngestOutcome) {
    match outcome {
        IngestOutcome::NotProjectable => tracing::debug!(
            target: "magician::api_mining",
            origin,
            capability_id,
            status,
            "api_mining.recipe_feedback.projection_not_projectable"
        ),
        IngestOutcome::PendingCreated { projection_id } => tracing::info!(
            target: "magician::api_mining",
            origin,
            capability_id,
            status,
            projection_id,
            "api_mining.recipe_feedback.projection_pending_created"
        ),
        IngestOutcome::PendingExisting { projection_id } => tracing::debug!(
            target: "magician::api_mining",
            origin,
            capability_id,
            status,
            projection_id,
            "api_mining.recipe_feedback.projection_pending_existing"
        ),
        IngestOutcome::Ingested {
            projection_id,
            row_count,
            schema_added_columns,
        } => tracing::info!(
            target: "magician::api_mining",
            origin,
            capability_id,
            status,
            projection_id,
            row_count,
            schema_added_columns,
            "api_mining.recipe_feedback.projection_ingested"
        ),
        IngestOutcome::MigrationRejected {
            projection_id,
            reason,
        } => tracing::warn!(
            target: "magician::api_mining",
            origin,
            capability_id,
            status,
            projection_id,
            reason,
            "api_mining.recipe_feedback.projection_migration_rejected"
        ),
        IngestOutcome::Failed { reason } => tracing::warn!(
            target: "magician::api_mining",
            origin,
            capability_id,
            status,
            reason,
            "api_mining.recipe_feedback.projection_failed"
        ),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::capability::{ApiCapability, ConfidenceLevel};

    #[test]
    fn auth_feedback_never_demotes_capability() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(temp.path());
        let base = workspace_layout.api_mining_root("agent", "workspace");
        let mut registry = CapabilityRegistry::with_base_path(&base).unwrap();
        let mut capability = ApiCapability::new(
            "list".into(),
            "https://example.com".into(),
            "GET".into(),
            "https://example.com/items".into(),
        );
        capability.confidence = ConfidenceLevel::Trusted;
        let id = capability.id.clone();
        registry.register(&capability).unwrap();

        let sink = RecipeFeedbackSink::for_scope(&workspace_layout, "agent", "workspace").unwrap();
        sink.record(
            "https://example.com",
            &id,
            "https://example.com/items",
            false,
            true,
            401,
            "{}",
        );

        let stored = CapabilityRegistry::with_base_path(&base)
            .unwrap()
            .get_capability("https://example.com", &id)
            .unwrap();
        assert_eq!(stored.confidence, ConfidenceLevel::Trusted);
        assert_eq!(stored.auth_failure_count, 1);
        assert_eq!(stored.replay_failure_count, 0);
    }

    #[test]
    fn destructive_cleanup_forgets_cached_feedback_handles() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(temp.path());
        let unique = ulid::Ulid::new().to_string();
        let principal = format!("feedback-purge-{unique}");
        let workspace = "default";
        let first =
            RecipeFeedbackSink::for_scope(&workspace_layout, &principal, workspace).unwrap();
        assert!(RecipeFeedbackSink::forget_scope(
            &workspace_layout,
            &principal,
            workspace,
        ));
        assert!(!RecipeFeedbackSink::forget_scope(
            &workspace_layout,
            &principal,
            workspace,
        ));
        let reopened =
            RecipeFeedbackSink::for_scope(&workspace_layout, &principal, workspace).unwrap();
        assert!(!Arc::ptr_eq(&first.state, &reopened.state));
        RecipeFeedbackSink::forget_scope(&workspace_layout, &principal, workspace);
        super::super::projection_pipeline::forget_pipeline_for_scope(&principal, workspace);
    }
}
