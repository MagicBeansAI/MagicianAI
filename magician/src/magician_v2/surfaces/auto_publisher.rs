//! Auto-surface publication engine.
//!
//! After an agent completes an episode, `AutoSurfacePublisher::maybe_publish_for_task`
//! writes a scoped V3 publication record that points at the canonical `task + user`
//! output for the task. The publication record is the active write-side owner for
//! published surfaces on the V3 path.
//!
//! A companion helper, `materialize_dashboardable_artifacts`, still persists
//! dashboardable execution artifacts to the durable store for legacy artifact
//! registration paths. It is no longer used as publication truth.

use std::sync::Arc;

use chrono::Utc;
use tracing::warn;

use super::renderable_types;
use crate::magician_v2::agents::storage::sanitize_segment;
use crate::magician_v2::agents::types::{AgentDefinition, AutoSurfacePolicy};
use crate::magician_v2::artifact_v2::{
    memory::V3EpisodeRecord,
    models::{PublishSurfaceInput, PublishedSurfacePlacement, PublishedSurfaceRecord},
    publications::{
        published_surface_changed_payload, FilesystemPublishedSurfaceStore,
        PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
    },
    service::{ArtifactV2Error, ArtifactV2Service, ScopeRef},
    workspace::ArtifactV2Workspace,
};
use crate::magician_v2::artifacts::durable_store::{DurableArtifactStore, DurableFrontmatter};
use crate::magician_v2::execution::agentic::{
    types::bounded_artifact_json_is_valid, Artifact as ExecutionArtifact,
};
use crate::magician_v2::realtime_events::{AgentEventEnvelope, RuntimeTransportBroadcaster};
use crate::magician_v2::storage::task_models::Task;

// ---------------------------------------------------------------------------
// AutoSurfacePublisher
// ---------------------------------------------------------------------------

pub struct AutoSurfacePublisher {
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    publication_store: Option<FilesystemPublishedSurfaceStore>,
    publication_service: Option<Arc<ArtifactV2Service>>,
}

impl Default for AutoSurfacePublisher {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoSurfacePublisher {
    pub fn new() -> Self {
        Self {
            event_broadcaster: None,
            publication_store: None,
            publication_service: None,
        }
    }

    pub fn with_event_broadcaster(mut self, broadcaster: Arc<RuntimeTransportBroadcaster>) -> Self {
        self.event_broadcaster = Some(broadcaster);
        self
    }

    pub fn with_publication_workspace(mut self, workspace: ArtifactV2Workspace) -> Self {
        self.publication_store = Some(FilesystemPublishedSurfaceStore::new(workspace));
        self
    }

    pub fn with_publication_service(mut self, service: Arc<ArtifactV2Service>) -> Self {
        self.publication_service = Some(service);
        self
    }

    /// Inspect the agent definition and task, and if auto-surface is enabled,
    /// publish the canonical task-user output for the episode into the scoped
    /// V3 publication store.
    pub async fn maybe_publish_for_task(
        &self,
        agent_def: &AgentDefinition,
        task: &Task,
        episode: &V3EpisodeRecord,
    ) -> Vec<PublishedSurfaceRecord> {
        let policy = task
            .auto_surface_policy
            .as_ref()
            .or(agent_def.auto_surface_policy.as_ref());
        let policy = match policy {
            Some(p) if p.enabled => p,
            _ => return Vec::new(),
        };
        let Some(input) = build_publish_surface_input(policy, agent_def, task, episode) else {
            return Vec::new();
        };

        if let Some(service) = self.publication_service.as_ref() {
            let scope = ScopeRef::system_internal_unauthenticated(
                &task.principal.clone(),
                &task.workspace.clone(),
            );
            return match service.publish_surface_record(&scope, input).await {
                Ok(record) => vec![record],
                Err(err) => {
                    warn!(
                        task_id = %task.id,
                        agent_id = %task.agent_id,
                        error = %err,
                        "auto-surface: failed to publish through V3 publication service"
                    );
                    Vec::new()
                },
            };
        }

        let Some(store) = self.publication_store.as_ref() else {
            warn!(
                task_id = %task.id,
                agent_id = %task.agent_id,
                "auto-surface: scoped publication store unavailable"
            );
            return Vec::new();
        };

        let route = input.route.as_deref().unwrap_or("/briefing");
        let source_output_id = input.source_output_id.clone().unwrap_or_default();
        let logical_surface_id = input.logical_surface_id.clone().unwrap_or_else(|| {
            fallback_logical_surface_id(task, route, input.surface_kind.as_deref())
        });
        let surface_kind = input
            .surface_kind
            .clone()
            .unwrap_or_else(|| derive_surface_kind_from_route(route));

        let summary = match episode.outcome_summary_text().trim() {
            "" => Some(format!("Published output for task \"{}\"", task.title)),
            value => Some(value.to_string()),
        };
        let surface_id = format!("pubsurf_{}", uuid::Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();
        let record = PublishedSurfaceRecord {
            surface_id: surface_id.clone(),
            principal: task.principal.clone(),
            workspace: task.workspace.clone(),
            surface_kind,
            status: "active".to_string(),
            logical_surface_id: Some(logical_surface_id),
            route: route.to_string(),
            document_key: format!("published-surface-{}", sanitize_segment(&surface_id)),
            task_id: Some(task.id.clone()),
            ui_thread_id: Some(task.ui_thread_id.clone()),
            source_output_id: Some(source_output_id),
            source_execution_id: episode
                .execution_id
                .clone()
                .or_else(|| task.active_root_execution_id.clone()),
            media_type: None,
            materialized_render_kind: None,
            materialized_document_key: None,
            materialized_at: None,
            title: build_title(policy, agent_def, task),
            summary,
            placement: input
                .placement
                .unwrap_or_else(|| fallback_publication_placement(policy, task)),
            manifest_artifact_uid: None,
            manifest_name: None,
            input_artifact_ids: Vec::new(),
            published_at: now.clone(),
            unpublished_at: None,
            updated_at: now,
        };

        let record = match store.upsert_surface(&record).await {
            Ok(record) => record,
            Err(err) => {
                warn!(
                    task_id = %task.id,
                    surface_id = %surface_id,
                    error = %err,
                    "auto-surface: failed to persist scoped V3 publication record"
                );
                return Vec::new();
            },
        };

        let superseded_records =
            if let Some(logical_surface_id) = record.logical_surface_id.as_deref() {
                match supersede_logical_surface_predecessors(
                    store,
                    &task.principal,
                    &task.workspace,
                    logical_surface_id,
                    &record.surface_id,
                )
                .await
                {
                    Ok(records) => records,
                    Err(err) => {
                        warn!(
                            task_id = %task.id,
                            surface_id = %record.surface_id,
                            logical_surface_id = %logical_surface_id,
                            error = %err,
                            "auto-surface: failed to supersede predecessor publication records"
                        );
                        Vec::new()
                    },
                }
            } else {
                Vec::new()
            };

        if let Some(broadcaster) = self.event_broadcaster.as_ref() {
            for superseded in &superseded_records {
                broadcaster.emit_agent_transport_event(AgentEventEnvelope::new(
                    PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
                    &task.agent_id,
                    published_surface_changed_payload(superseded),
                ));
            }
            broadcaster.emit_agent_transport_event(AgentEventEnvelope::new(
                PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
                &task.agent_id,
                published_surface_changed_payload(&record),
            ));
        }

        vec![record]
    }
}

async fn supersede_logical_surface_predecessors(
    store: &FilesystemPublishedSurfaceStore,
    principal: &str,
    workspace: &str,
    logical_surface_id: &str,
    current_surface_id: &str,
) -> Result<Vec<PublishedSurfaceRecord>, ArtifactV2Error> {
    let surfaces = store.list_surfaces(principal, workspace).await?;
    let mut superseded = Vec::new();
    for surface in surfaces {
        if surface.surface_id == current_surface_id {
            continue;
        }
        if surface.status != "active" {
            continue;
        }
        if surface.logical_surface_id.as_deref() != Some(logical_surface_id) {
            continue;
        }
        if let Some(record) = store
            .mark_surface_superseded(principal, workspace, &surface.surface_id)
            .await?
        {
            superseded.push(record);
        }
    }
    Ok(superseded)
}

fn build_publish_surface_input(
    policy: &AutoSurfacePolicy,
    agent_def: &AgentDefinition,
    task: &Task,
    episode: &V3EpisodeRecord,
) -> Option<PublishSurfaceInput> {
    let source_output_id = episode.task_user_output_id.clone()?;
    let route = policy.route.trim();
    if route.is_empty() {
        warn!(
            task_id = %task.id,
            agent_id = %task.agent_id,
            "auto-surface: policy route was empty"
        );
        return None;
    }

    let surface_kind = policy
        .surface_kind
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .or_else(|| Some(derive_surface_kind_from_route(route)));
    let summary = match episode.outcome_summary_text().trim() {
        "" => Some(format!("Published output for task \"{}\"", task.title)),
        value => Some(value.to_string()),
    };

    Some(PublishSurfaceInput {
        task_id: task.id.clone(),
        source_output_id: Some(source_output_id),
        materialize_as: policy
            .materialize_as
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string()),
        logical_surface_id: Some(fallback_logical_surface_id(
            task,
            route,
            surface_kind.as_deref(),
        )),
        surface_kind,
        route: Some(route.to_string()),
        title: Some(build_title(policy, agent_def, task)),
        summary,
        placement: Some(fallback_publication_placement(policy, task)),
    })
}

fn derive_surface_kind_from_route(route: &str) -> String {
    match route {
        "/briefing" => "briefing".to_string(),
        other if other.contains("dashboard") => "dashboard".to_string(),
        _ => "published_surface".to_string(),
    }
}

fn fallback_logical_surface_id(task: &Task, route: &str, surface_kind: Option<&str>) -> String {
    if matches!(surface_kind, Some("dashboard")) {
        format!("task:{}:{}:dashboard", task.id, route)
    } else {
        format!("task:{}:{}", task.id, route)
    }
}

fn fallback_publication_placement(
    policy: &AutoSurfacePolicy,
    task: &Task,
) -> PublishedSurfacePlacement {
    let placement_kind = policy
        .placement_kind
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or("task")
        .to_string();
    let placement_id = match placement_kind.as_str() {
        "task" => Some(task.id.clone()),
        "thread" => Some(task.ui_thread_id.clone()),
        "workspace" => Some(task.workspace.clone()),
        "global" => None,
        _ => Some(task.id.clone()),
    };

    PublishedSurfacePlacement {
        placement_kind,
        placement_id,
        pinned: policy.pinned.unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// Title builder
// ---------------------------------------------------------------------------

/// Expand the policy title template with available context values.
///
/// Supported placeholders:
/// - `{agent_name}` — the agent's display name
/// - `{goal}` / `{task_title}` — the task title
/// - `{agent_id}` — the agent ID
fn build_title(policy: &AutoSurfacePolicy, agent_def: &AgentDefinition, task: &Task) -> String {
    policy
        .title_template
        .replace("{agent_name}", &agent_def.name)
        .replace("{goal}", &task.title)
        .replace("{task_title}", &task.title)
        .replace("{agent_id}", &agent_def.agent_id)
}

// ---------------------------------------------------------------------------
// Materialization helper
// ---------------------------------------------------------------------------

/// Persist dashboardable execution artifacts to the durable store so that the
/// surface publisher can later reference them via `PhysicalLocator::DurableStore`.
///
/// For each artifact whose `artifact_type` is dashboardable, the raw data bytes
/// are written to `durable_store` under the `execution_artifacts` namespace.
/// Non-dashboardable artifacts are ignored. This helper deliberately returns
/// no rewritten artifact collection: its only production caller needs the
/// durable side effect, and cloning every payload here used to double the live
/// bytes for large successful executions.
pub async fn materialize_dashboardable_artifacts(
    artifacts: &[ExecutionArtifact],
    agent_id: &str,
    cycle_id: &str,
    durable_store: &DurableArtifactStore,
    task_id: Option<&str>,
    execution_id: Option<&str>,
) {
    const MAX_DASHBOARDABLE_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

    for artifact in artifacts {
        let is_dashboardable = artifact
            .artifact_type
            .as_deref()
            .map(renderable_types::is_dashboardable)
            .unwrap_or(false);

        if !is_dashboardable {
            continue;
        }

        // Validate without materializing a second complete Value tree. The
        // durable store accepts the already-canonical UTF-8 bytes produced by
        // `Artifact::json`; pretty reserialization was presentation-only and
        // doubled peak memory on this background path.
        if !bounded_artifact_json_is_valid(&artifact.data, MAX_DASHBOARDABLE_ARTIFACT_BYTES) {
            warn!(
                artifact_name = %artifact.name,
                agent_id = %agent_id,
                max_bytes = MAX_DASHBOARDABLE_ARTIFACT_BYTES,
                "materialize_dashboardable: data is invalid, too deep, or oversized JSON; skipping"
            );
            continue;
        }
        let Ok(content_str) = std::str::from_utf8(&artifact.data) else {
            warn!(
                artifact_name = %artifact.name,
                agent_id = %agent_id,
                "materialize_dashboardable: JSON bytes are not UTF-8; skipping"
            );
            continue;
        };

        let store_name = format!("{}-{}-{}.json", agent_id, cycle_id, artifact.name);

        let frontmatter = DurableFrontmatter {
            namespace: "execution_artifacts".to_string(),
            name: store_name.clone(),
            created_by: agent_id.to_string(),
            last_updated_by: agent_id.to_string(),
            last_updated: Utc::now(),
            content_type: Some(artifact.content_type.clone()),
            source_execution_id: execution_id.map(String::from),
            source_task_id: task_id.map(String::from),
            source_workflow_instance_id: None,
            source_run_id: None,
            source_cycle_id: Some(cycle_id.to_string()),
            source_agent_id: Some(agent_id.to_string()),
            producer_stage: Some("execution".to_string()),
        };

        match durable_store
            .write("execution_artifacts", &store_name, content_str, frontmatter)
            .await
        {
            Ok(_path) => {},
            Err(e) => {
                warn!(
                    artifact_name = %artifact.name,
                    agent_id = %agent_id,
                    "materialize_dashboardable: durable write failed: {e}"
                );
            },
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::Arc;

    use super::{
        build_publish_surface_input, materialize_dashboardable_artifacts, AutoSurfacePublisher,
    };
    use crate::magician_v2::agents::{
        memory::EpisodeOutcome, types::AutoSurfacePolicy, AgentDefinition,
    };
    use crate::magician_v2::artifact_v2::{
        memory::V3EpisodeRecord, publications::FilesystemPublishedSurfaceStore,
        workspace::ArtifactV2Workspace,
    };
    use crate::magician_v2::artifacts::durable_store::DurableArtifactStore;
    use crate::magician_v2::execution::agentic::Artifact;
    use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};
    use crate::magician_v2::storage::task_models::{Task, TaskStatus};
    use chrono::{Duration, Utc};
    use tokio::time::{timeout, Duration as TokioDuration};

    fn parse_agent_definition() -> AgentDefinition {
        AgentDefinition::from_yaml_str(
            r#"
agent_id: "agent-1"
name: "Agent One"
persona: "Test"
tools: []
auto_surface_policy:
  enabled: true
  route: "/briefing"
  title_template: "Agent Dashboard"
"#,
        )
        .unwrap()
    }

    fn sample_task() -> Task {
        let now = Utc::now().timestamp_millis();
        Task {
            id: "task-1".to_string(),
            principal: "user-1".to_string(),
            workspace: "/tmp".to_string(),
            ui_thread_id: crate::magician_v2::storage::task_models::default_ui_thread_id(),
            title: "Review metrics".to_string(),
            description: "Build a dashboard".to_string(),
            status: TaskStatus::Completed,
            priority: None,
            due_date: None,
            tags: Vec::new(),
            agent_id: "agent-1".to_string(),
            schedule: None,
            created_by: Default::default(),
            depends_on: Vec::new(),
            linked_task_ids: Vec::new(),
            approved: true,
            has_plan: false,
            active_root_execution_id: Some("exec-1".to_string()),
            latest_root_execution_id: Some("exec-1".to_string()),
            last_completed_root_execution_id: Some("exec-1".to_string()),
            error_message: None,
            retry_at: None,
            current_step: None,
            progress: None,
            completion_summary: None,
            completion_outcome: None,
            completion_artifact_names: None,
            auto_surface_policy: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn sample_episode() -> V3EpisodeRecord {
        let now = Utc::now();
        let mut episode = V3EpisodeRecord::new_memory_episode(
            None,
            "agent-1",
            "ep-1",
            "task-1",
            "task_completed",
            1,
            now,
            None,
            now - Duration::seconds(5),
            now,
            &EpisodeOutcome::GoalAchieved {
                summary: "done".to_string(),
            },
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
        );
        episode.task_id = Some("task-1".to_string());
        episode.execution_id = Some("exec-1".to_string());
        episode.task_user_output_id = Some("out_task_user_user_output".to_string());
        episode
    }

    #[test]
    fn build_publish_surface_input_honors_dashboard_materialization_policy() {
        let task = sample_task();
        let episode = sample_episode();
        let policy = AutoSurfacePolicy {
            enabled: true,
            route: "/briefing".to_string(),
            materialize_as: Some("muij_surface".to_string()),
            surface_kind: Some("dashboard".to_string()),
            placement_kind: Some("workspace".to_string()),
            pinned: Some(true),
            title_template: "{agent_name}: {goal}".to_string(),
        };
        let agent_def = parse_agent_definition();

        let input = build_publish_surface_input(&policy, &agent_def, &task, &episode)
            .expect("publish input");

        assert_eq!(input.materialize_as.as_deref(), Some("muij_surface"));
        assert_eq!(input.surface_kind.as_deref(), Some("dashboard"));
        assert_eq!(
            input.logical_surface_id.as_deref(),
            Some("task:task-1:/briefing:dashboard")
        );
        let placement = input.placement.expect("placement");
        assert_eq!(placement.placement_kind, "workspace");
        assert_eq!(placement.placement_id.as_deref(), Some("/tmp"));
        assert!(placement.pinned);
    }

    #[tokio::test]
    async fn dashboard_materialization_persists_only_dashboardable_payloads_without_rewriting_inputs(
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = DurableArtifactStore::new(tmp.path().join("artifacts"))
            .expect("durable artifact store");
        let dashboard = Artifact {
            name: "metrics".to_string(),
            content_type: "application/json".to_string(),
            data: br#"{"items":[{"value":1}]}"#.to_vec(),
            artifact_type: Some("custom:metric_set".to_string()),
            render_hints: None,
            materialized_path: None,
        };
        let ordinary = Artifact::text("notes", "not dashboardable");
        let artifacts = vec![dashboard, ordinary];
        let original_payload_ptr = artifacts[0].data.as_ptr();

        materialize_dashboardable_artifacts(
            &artifacts,
            "agent",
            "cycle",
            &store,
            Some("task"),
            Some("execution"),
        )
        .await;

        let (frontmatter, body) = store
            .read("execution_artifacts", "agent-cycle-metrics.json")
            .await
            .expect("dashboard artifact persisted");
        assert_eq!(body, r#"{"items":[{"value":1}]}"#);
        assert_eq!(frontmatter.source_task_id.as_deref(), Some("task"));
        assert_eq!(artifacts[0].data.as_ptr(), original_payload_ptr);
        assert_eq!(
            store
                .list(Some("execution_artifacts"))
                .expect("artifact listing")
                .len(),
            1,
            "non-dashboardable artifacts are not copied into the durable store",
        );
    }

    #[tokio::test]
    async fn maybe_publish_writes_scoped_v3_record_and_supersedes_predecessor() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path().join("magician_data_v3"));
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut events = broadcaster.subscribe();
        let publisher = AutoSurfacePublisher::new()
            .with_publication_workspace(workspace.clone())
            .with_event_broadcaster(Arc::clone(&broadcaster));
        let store = FilesystemPublishedSurfaceStore::new(workspace);
        let task = sample_task();
        let episode = sample_episode();
        let agent_def = parse_agent_definition();

        let first = publisher
            .maybe_publish_for_task(&agent_def, &task, &episode)
            .await;
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].source_output_id.as_deref(),
            Some("out_task_user_user_output")
        );
        assert!(first[0].manifest_artifact_uid.is_none());
        assert!(first[0].manifest_name.is_none());
        let first_event = timeout(TokioDuration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_published_surface_event_status(&first_event, &first[0].surface_id, "active");

        let second = publisher
            .maybe_publish_for_task(&agent_def, &task, &episode)
            .await;
        assert_eq!(second.len(), 1);
        let superseded_event = timeout(TokioDuration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_published_surface_event_status(
            &superseded_event,
            &first[0].surface_id,
            "superseded",
        );
        let active_event = timeout(TokioDuration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_published_surface_event_status(&active_event, &second[0].surface_id, "active");

        let stored = store
            .list_surfaces(&task.principal, &task.workspace)
            .await
            .unwrap();
        assert_eq!(stored.len(), 2);

        let active = stored
            .iter()
            .find(|record| record.status == "active")
            .expect("active publication");
        let superseded = stored
            .iter()
            .find(|record| record.status == "superseded")
            .expect("superseded predecessor");

        assert_eq!(active.surface_id, second[0].surface_id);
        assert_eq!(superseded.surface_id, first[0].surface_id);
        assert_eq!(
            active.logical_surface_id.as_deref(),
            Some("task:task-1:/briefing")
        );
    }

    fn assert_published_surface_event_status(
        event: &RuntimeTransportEvent,
        surface_id: &str,
        status: &str,
    ) {
        let RuntimeTransportEvent::AgentEvent { event } = event else {
            panic!("expected AgentEvent");
        };
        assert_eq!(
            event.event_type,
            super::PUBLISHED_SURFACE_CHANGED_EVENT_TYPE
        );
        assert_eq!(
            event
                .payload
                .get("surface_id")
                .and_then(|value| value.as_str()),
            Some(surface_id)
        );
        assert_eq!(
            event.payload.get("status").and_then(|value| value.as_str()),
            Some(status)
        );
    }
}
