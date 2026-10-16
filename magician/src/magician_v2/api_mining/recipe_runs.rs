//! Durable, bounded run history for task recipes.

use super::path_safe::ensure_safe_record_id;
use super::recipe::TaskRecipe;
use super::recipe_runner::{FailureClass, RecipeRunResult, RecipeStepOutcome};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::artifact_v2::ArtifactV2Error;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

const MAX_LIST_LIMIT: usize = 200;
const INITIAL_TAIL_BYTES: u64 = 128 * 1024;
const MAX_TAIL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_LEDGER_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_LEDGER_RECORD_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeRunRecord {
    pub ts_ms: i64,
    pub recipe_id: String,
    pub version: u32,
    pub task_id: String,
    pub execution_id: String,
    /// `api`, `api_then_browser`, `browser`, `denied`, or `verification`.
    pub rail_ended: String,
    pub steps: Vec<RecipeStepOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<FailureClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_step_id: Option<String>,
    pub auth_heals: u32,
    pub transport_downgrades: u32,
    #[serde(default)]
    pub approval_request_ids: Vec<String>,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recompiled_to_version: Option<u32>,
}

impl RecipeRunRecord {
    pub fn from_result(
        recipe: &TaskRecipe,
        result: &RecipeRunResult,
        task_id: impl Into<String>,
        execution_id: impl Into<String>,
        rail_ended: impl Into<String>,
        duration_ms: u64,
        approval_request_ids: Vec<String>,
    ) -> Self {
        Self {
            ts_ms: chrono::Utc::now().timestamp_millis(),
            recipe_id: recipe.id.clone(),
            version: recipe.current_version,
            task_id: task_id.into(),
            execution_id: execution_id.into(),
            rail_ended: rail_ended.into(),
            steps: structural_steps(recipe, result),
            failure_class: result
                .failure
                .as_ref()
                .map(|failure| failure.class)
                .or_else(|| result.fallback.as_ref().map(|fallback| fallback.class)),
            fallback_step_id: result
                .fallback
                .as_ref()
                .map(|fallback| fallback.step_id.clone())
                .or_else(|| {
                    result
                        .failure
                        .as_ref()
                        .map(|failure| failure.step_id.clone())
                }),
            auth_heals: result.auth_heals,
            transport_downgrades: result
                .steps
                .iter()
                .filter(|step| step.transport != super::recipe::Transport::Reqwest)
                .count()
                .try_into()
                .unwrap_or(u32::MAX),
            approval_request_ids,
            duration_ms,
            recompiled_to_version: None,
        }
    }

    pub fn denied(
        recipe: &TaskRecipe,
        task_id: impl Into<String>,
        execution_id: impl Into<String>,
        step_id: impl Into<String>,
        duration_ms: u64,
        approval_request_ids: Vec<String>,
    ) -> Self {
        Self {
            ts_ms: chrono::Utc::now().timestamp_millis(),
            recipe_id: recipe.id.clone(),
            version: recipe.current_version,
            task_id: task_id.into(),
            execution_id: execution_id.into(),
            rail_ended: "denied".into(),
            steps: Vec::new(),
            failure_class: Some(FailureClass::PolicyBlocked),
            fallback_step_id: Some(step_id.into()),
            auth_heals: 0,
            transport_downgrades: 0,
            approval_request_ids,
            duration_ms,
            recompiled_to_version: None,
        }
    }

    pub fn learned_from_browser(
        recipe: &TaskRecipe,
        task_id: impl Into<String>,
        execution_id: impl Into<String>,
        recompiled: bool,
    ) -> Self {
        Self {
            ts_ms: chrono::Utc::now().timestamp_millis(),
            recipe_id: recipe.id.clone(),
            version: recipe.current_version,
            task_id: task_id.into(),
            execution_id: execution_id.into(),
            rail_ended: "browser".into(),
            steps: Vec::new(),
            failure_class: None,
            fallback_step_id: None,
            auth_heals: 0,
            transport_downgrades: 0,
            approval_request_ids: Vec::new(),
            duration_ms: 0,
            recompiled_to_version: recompiled.then_some(recipe.current_version),
        }
    }
}

fn structural_steps(recipe: &TaskRecipe, result: &RecipeRunResult) -> Vec<RecipeStepOutcome> {
    let current = recipe.current();
    result
        .steps
        .iter()
        .map(|outcome| {
            let definition = current
                .and_then(|version| version.steps.iter().find(|step| step.id == outcome.step_id));
            RecipeStepOutcome {
                step_id: outcome.step_id.clone(),
                method: definition
                    .map(|step| step.method.clone())
                    .unwrap_or_else(|| outcome.method.clone()),
                url: definition
                    .map(|step| super::recipe_runner::redact_url(&step.url_template))
                    .unwrap_or_else(|| "<unknown-step>".into()),
                status: outcome.status,
                duration_ms: outcome.duration_ms,
                transport: outcome.transport,
                preview: None,
            }
        })
        .collect()
}

/// Publish the API-executed portion of a recipe run into the retained
/// capability-sequence stream. Request inputs and response bodies stay out of
/// this compatibility projection; recipe runs already have their own bounded
/// ledger, while sequences only need ordering, capability linkage, transport
/// path, status, and timing for downstream workflow/parity consumers.
pub fn persist_capability_sequence(
    mining_base: &Path,
    recipe: &TaskRecipe,
    result: &RecipeRunResult,
    task_id: &str,
    execution_id: &str,
) -> io::Result<Option<String>> {
    let Some(version) = recipe.current() else {
        return Ok(None);
    };
    let Some(origin_key) = version.origins.first() else {
        return Ok(None);
    };
    if result.steps.is_empty() {
        return Ok(None);
    }

    let started_at_ms = chrono::Utc::now().timestamp_millis();
    let mut recorder = super::sequence_recorder::SequenceRecorder::start(
        task_id,
        execution_id,
        origin_key,
        started_at_ms,
    );
    let mut elapsed_ms = 0_u64;
    for outcome in &result.steps {
        let Some(step) = version.steps.iter().find(|step| step.id == outcome.step_id) else {
            continue;
        };
        elapsed_ms = elapsed_ms.saturating_add(outcome.duration_ms);
        recorder.record_api_step(
            step.capability_id.clone(),
            &step.origin,
            &super::recipe_runner::redact_url(&step.url_template),
            &step.method,
            std::collections::HashMap::new(),
            None,
            outcome.status,
            None,
            None,
            started_at_ms.saturating_add(i64::try_from(elapsed_ms).unwrap_or(i64::MAX)),
            outcome.duration_ms,
        );
    }
    if recorder.step_count() == 0 {
        return Ok(None);
    }

    let sequence = recorder.finalize();
    let sequence_id = sequence.id.clone();
    super::sequence_store::SequenceStore::new(mining_base.to_path_buf()).save(&sequence)?;
    Ok(Some(sequence_id))
}

pub struct RecipeRunLedger {
    base: PathBuf,
    workspace_layout: ArtifactV2Workspace,
}

impl RecipeRunLedger {
    pub fn new(base: impl AsRef<Path>) -> Self {
        let base = base.as_ref().to_path_buf();
        Self {
            workspace_layout: ArtifactV2Workspace::with_local_file_provider(&base),
            base,
        }
    }

    fn path(&self, recipe_id: &str) -> io::Result<PathBuf> {
        ensure_safe_record_id(recipe_id, "recipe id")?;
        Ok(self.base.join("recipes").join(recipe_id).join("runs.jsonl"))
    }

    pub async fn append(&self, record: &RecipeRunRecord) -> io::Result<()> {
        let path = self.path(&record.recipe_id)?;
        if let Some(parent) = path.parent() {
            self.workspace_layout
                .create_dir_all_path_sync(parent)
                .map_err(io::Error::other)?;
        }
        let mut line = serde_json::to_vec(record).map_err(io::Error::other)?;
        line.push(b'\n');
        if line.len() > MAX_LEDGER_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recipe run record exceeds the durable ledger limit",
            ));
        }
        let current_bytes = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        if current_bytes.saturating_add(line.len() as u64) <= MAX_LEDGER_FILE_BYTES {
            return self
                .workspace_layout
                .append_path(path, &line)
                .await
                .map_err(io::Error::other);
        }

        // The replay transaction already serializes appends for one recipe.
        // At the byte threshold, retain as many newest complete records as fit
        // beside the new record and publish the compacted ledger atomically.
        let newest = self.list(&record.recipe_id, MAX_LIST_LIMIT).await?;
        let mut retained_newest_first = Vec::new();
        let mut retained_bytes = line.len();
        for candidate in newest {
            let candidate_bytes = serde_json::to_vec(&candidate)
                .map_err(io::Error::other)?
                .len()
                .saturating_add(1);
            let Some(next_bytes) = retained_bytes.checked_add(candidate_bytes) else {
                break;
            };
            if next_bytes > MAX_LEDGER_FILE_BYTES as usize {
                break;
            }
            retained_newest_first.push(candidate);
            retained_bytes = next_bytes;
        }
        retained_newest_first.reverse();
        retained_newest_first.push(record.clone());
        self.workspace_layout
            .write_jsonl_records_atomic_path(path, &retained_newest_first)
            .await
            .map_err(io::Error::other)
    }

    /// Newest first. A committed malformed line fails closed; silently
    /// skipping acknowledged ledger records would make audit history lie.
    pub async fn list(&self, recipe_id: &str, limit: usize) -> io::Result<Vec<RecipeRunRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let path = self.path(recipe_id)?;
        let limit = limit.min(MAX_LIST_LIMIT);
        let tail = match self
            .workspace_layout
            .read_jsonl_tail_recent_path::<RecipeRunRecord, _>(
                path,
                limit,
                INITIAL_TAIL_BYTES,
                MAX_TAIL_BYTES,
            )
            .await
        {
            Ok(tail) => tail,
            Err(ArtifactV2Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            },
            Err(error) => return Err(io::Error::other(error)),
        };
        let mut records = tail.records;
        records.reverse();
        Ok(records)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::capability::SideEffects;
    use crate::magician_v2::api_mining::recipe::*;
    use crate::magician_v2::api_mining::sequence::ExecutionPath;
    use std::collections::HashMap;

    fn record(recipe_id: &str, ts_ms: i64) -> RecipeRunRecord {
        RecipeRunRecord {
            ts_ms,
            recipe_id: recipe_id.into(),
            version: 1,
            task_id: "task".into(),
            execution_id: format!("exec_{ts_ms}"),
            rail_ended: "api".into(),
            steps: Vec::new(),
            failure_class: None,
            fallback_step_id: None,
            auth_heals: 0,
            transport_downgrades: 0,
            approval_request_ids: Vec::new(),
            duration_ms: 1,
            recompiled_to_version: None,
        }
    }

    #[tokio::test]
    async fn append_and_list_are_newest_first() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = RecipeRunLedger::new(temp.path());
        ledger.append(&record("rcp_1", 1)).await.unwrap();
        ledger.append(&record("rcp_1", 2)).await.unwrap();
        let records = ledger.list("rcp_1", 10).await.unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].ts_ms, 2);
        assert_eq!(records[1].ts_ms, 1);
    }

    #[tokio::test]
    async fn append_compacts_the_ledger_to_its_byte_budget() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = RecipeRunLedger::new(temp.path());
        for timestamp in 0..20 {
            let mut entry = record("rcp_compact", timestamp);
            entry.task_id = "x".repeat(220 * 1024);
            ledger.append(&entry).await.unwrap();
        }

        let path = temp.path().join("recipes/rcp_compact/runs.jsonl");
        assert!(std::fs::metadata(path).unwrap().len() <= MAX_LEDGER_FILE_BYTES);
        let records = ledger.list("rcp_compact", MAX_LIST_LIMIT).await.unwrap();
        assert_eq!(records.first().map(|record| record.ts_ms), Some(19));
    }

    #[tokio::test]
    async fn oversized_ledger_records_are_rejected_before_append() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = RecipeRunLedger::new(temp.path());
        let mut entry = record("rcp_oversized", 1);
        entry.task_id = "x".repeat(MAX_LEDGER_RECORD_BYTES);

        assert!(ledger.append(&entry).await.is_err());
        assert!(!temp
            .path()
            .join("recipes/rcp_oversized/runs.jsonl")
            .exists());
    }

    #[test]
    fn recipe_replay_sequence_is_structural_and_api_only() {
        let temp = tempfile::tempdir().unwrap();
        let recipe = TaskRecipe {
            id: "rcp_1".into(),
            scope_principal: "owner".into(),
            scope_workspace: "default".into(),
            agent_id: "assistant".into(),
            shape: RecipeShape {
                description_template: None,
                template: "read item {item}".into(),
                fingerprint: "shape".into(),
                inputs: Vec::new(),
            },
            current_version: 1,
            versions: vec![RecipeVersion {
                version: 1,
                origins: vec!["https://example.test".into()],
                steps: vec![RecipeStep {
                    id: "s0".into(),
                    origin: "https://example.test".into(),
                    method: "GET".into(),
                    url_template: "https://example.test/api/items/{item}".into(),
                    headers_template: HashMap::new(),
                    body_template: None,
                    capability_id: Some("cap_items".into()),
                    param_sources: HashMap::new(),
                    body_param_types: HashMap::new(),
                    side_effects: SideEffects::ReadOnly,
                    request_shape_fingerprint: "request-shape".into(),
                    verify_with: None,
                    browser_fallback: None,
                    transport_hint: None,
                }],
                data_flows: Vec::new(),
                answer_spec: Vec::new(),
                auth: RecipeAuth::default(),
                maturity: RecipeMaturity::Draft,
                replay_stats: Default::default(),
                compiled_from: CompiledFrom {
                    task_id: "task_1".into(),
                    execution_id: "exec_compile".into(),
                    task_text_fingerprint: None,
                    monitor_revision: None,
                    sequence_ids: Vec::new(),
                    trace_files: Vec::new(),
                },
                compiled_at_ms: 1,
                last_replayed_at_ms: None,
            }],
        };
        let result = RecipeRunResult {
            success: true,
            auth_heals: 0,
            answer: serde_json::Map::new(),
            steps: vec![RecipeStepOutcome {
                step_id: "s0".into(),
                method: "GET".into(),
                url: "https://example.test/api/items/42?token=[REDACTED]".into(),
                status: Some(200),
                duration_ms: 9,
                transport: Transport::Reqwest,
                preview: None,
            }],
            failure: None,
            fallback: None,
            pending_approval: None,
        };

        let sequence_id =
            persist_capability_sequence(temp.path(), &recipe, &result, "task_1", "exec_1")
                .unwrap()
                .unwrap();
        let sequence = super::super::sequence_store::SequenceStore::new(temp.path().to_path_buf())
            .load("example.test", &sequence_id)
            .unwrap()
            .unwrap();

        assert_eq!(sequence.task_id, "task_1");
        assert_eq!(sequence.execution_id, "exec_1");
        assert_eq!(sequence.steps.len(), 1);
        assert_eq!(sequence.steps[0].executed_via, ExecutionPath::ApiReplay);
        assert_eq!(
            sequence.steps[0].capability_id.as_deref(),
            Some("cap_items")
        );
        assert!(sequence.steps[0].request_params.is_empty());
        assert!(sequence.steps[0].request_body.is_none());
        assert!(sequence.steps[0].response_body.is_none());
        assert!(!sequence.steps[0].concrete_url.contains("42"));
        assert!(!sequence.steps[0].concrete_url.contains("token="));
    }

    #[test]
    fn run_record_scrubs_untrusted_outcome_content_at_the_persistence_boundary() {
        let mut recipe = TaskRecipe {
            id: "rcp_scrub".into(),
            scope_principal: "owner".into(),
            scope_workspace: "default".into(),
            agent_id: "assistant".into(),
            shape: RecipeShape {
                description_template: None,
                template: "read item {item}".into(),
                fingerprint: "shape".into(),
                inputs: Vec::new(),
            },
            current_version: 1,
            versions: Vec::new(),
        };
        recipe.versions.push(RecipeVersion {
            version: 1,
            origins: vec!["https://example.test".into()],
            steps: vec![RecipeStep {
                id: "s0".into(),
                origin: "https://example.test".into(),
                method: "GET".into(),
                url_template: "https://example.test/api/items/{item}?token={token}".into(),
                headers_template: HashMap::new(),
                body_template: None,
                capability_id: Some("cap_items".into()),
                param_sources: HashMap::new(),
                body_param_types: HashMap::new(),
                side_effects: SideEffects::ReadOnly,
                request_shape_fingerprint: "request-shape".into(),
                verify_with: None,
                browser_fallback: None,
                transport_hint: None,
            }],
            data_flows: Vec::new(),
            answer_spec: Vec::new(),
            auth: RecipeAuth::default(),
            maturity: RecipeMaturity::Candidate,
            replay_stats: Default::default(),
            compiled_from: CompiledFrom {
                task_id: "task".into(),
                execution_id: "execution".into(),
                task_text_fingerprint: None,
                monitor_revision: None,
                sequence_ids: Vec::new(),
                trace_files: Vec::new(),
            },
            compiled_at_ms: 1,
            last_replayed_at_ms: None,
        });
        let result = RecipeRunResult {
            success: true,
            auth_heals: 0,
            answer: serde_json::Map::new(),
            steps: vec![RecipeStepOutcome {
                step_id: "s0".into(),
                method: "GET".into(),
                url: "https://example.test/api/items/private-user?token=response-secret".into(),
                status: Some(200),
                duration_ms: 1,
                transport: Transport::Reqwest,
                preview: Some("response-secret".into()),
            }],
            failure: None,
            fallback: None,
            pending_approval: None,
        };

        let record = RecipeRunRecord::from_result(
            &recipe,
            &result,
            "task",
            "execution",
            "api",
            1,
            Vec::new(),
        );
        let encoded = serde_json::to_string(&record).unwrap();
        assert!(!encoded.contains("private-user"));
        assert!(!encoded.contains("response-secret"));
        assert!(record.steps[0].preview.is_none());
    }
}
