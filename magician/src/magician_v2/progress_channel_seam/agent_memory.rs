use std::sync::Arc;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use serde_json::json;

use crate::magician_v2::agents::{
    memory::{AgentMemoryService, EpisodeOutcome},
    memory_tiers::TierScope,
    AgentDefinitionStore, AgentMemoryResolver,
};
use crate::magician_v2::artifact_v2::memory::{V3EpisodeRecord, V3MemoryTierRecord};

use crate::magician_v2::progress_channel_seam::{
    channel::ProgressChannel,
    surface_routing::agent_memory_surface_renders_agent_event,
    types::{ProgressMessage, ProgressMessageKind, ProgressSource, Subscription},
};

#[derive(Clone)]
pub struct AgentMemoryChannel {
    memory_resolver: Arc<AgentMemoryResolver>,
    definition_store: Arc<AgentDefinitionStore>,
}

impl AgentMemoryChannel {
    pub fn new(
        memory_resolver: Arc<AgentMemoryResolver>,
        definition_store: Arc<AgentDefinitionStore>,
    ) -> Self {
        Self {
            memory_resolver,
            definition_store,
        }
    }

    fn message_time(message: &ProgressMessage) -> chrono::DateTime<Utc> {
        Utc.timestamp_millis_opt(message.timestamp)
            .single()
            .unwrap_or_else(Utc::now)
    }

    fn goal_id(message: &ProgressMessage, agent_id: &str) -> String {
        message
            .root_task_id
            .clone()
            .or_else(|| message.task_id.clone())
            .or_else(|| message.execution_id.clone())
            .unwrap_or_else(|| format!("agent:{agent_id}"))
    }

    fn episode_outcome(message: &ProgressMessage) -> Option<EpisodeOutcome> {
        match &message.kind {
            ProgressMessageKind::StatusChanged { status, summary }
            | ProgressMessageKind::ChildStatusChanged {
                status, summary, ..
            } => {
                if !matches!(status.as_str(), "completed" | "failed" | "cancelled") {
                    return None;
                }
                if status == "completed" {
                    Some(EpisodeOutcome::GoalAchieved {
                        summary: summary
                            .clone()
                            .unwrap_or_else(|| "Progress channel completion".to_string()),
                    })
                } else {
                    Some(EpisodeOutcome::Failed {
                        error: summary
                            .clone()
                            .unwrap_or_else(|| format!("Execution ended with status {status}")),
                    })
                }
            },
            ProgressMessageKind::AgentNotification {
                event_type,
                message,
                ..
            } => Some(EpisodeOutcome::PartialProgress {
                summary: message.clone(),
                remaining: event_type.clone(),
            }),
            _ => None,
        }
    }

    async fn persist_episode(
        &self,
        memory_service: &AgentMemoryService,
        agent_id: &str,
        message: &ProgressMessage,
    ) -> anyhow::Result<()> {
        let Some(outcome) = Self::episode_outcome(message) else {
            return Ok(());
        };
        let timestamp = Self::message_time(message);
        let goal_id = Self::goal_id(message, agent_id);
        let native_episode = V3EpisodeRecord::new_memory_episode(
            memory_service.scoped_memory_scope(),
            agent_id,
            format!("progress-{}", message.id),
            goal_id,
            "progress_channel",
            message.seq,
            timestamp,
            Some(
                serde_json::to_value(message)
                    .context("failed to serialize progress message for memory episode")?,
            ),
            timestamp,
            timestamp,
            &outcome,
            Vec::new(),
            vec![format!("progress_channel:{}", message.id)],
            Vec::new(),
            None,
            Some(format!(
                "{}:{}:{}",
                message.principal, message.workspace, message.log_key
            )),
            None,
        );
        memory_service
            .append_native_episode(agent_id, &native_episode)
            .await
            .context("failed to append agent memory episode")?;
        Ok(())
    }

    async fn persist_tier(
        &self,
        memory_service: &AgentMemoryService,
        agent_id: &str,
        tier_name: &str,
        message: &ProgressMessage,
    ) -> anyhow::Result<()> {
        let definition = self
            .definition_store
            .get_definition(agent_id)
            .await
            .with_context(|| format!("failed to load definition for `{agent_id}`"))?;
        let definition =
            definition.ok_or_else(|| anyhow!("agent definition `{agent_id}` not found"))?;
        let tier_definition = definition
            .definition
            .memory_tiers
            .iter()
            .find(|tier| tier.name == tier_name)
            .ok_or_else(|| anyhow!("tier definition `{tier_name}` not found for `{agent_id}`"))?;
        let goal_id = Self::goal_id(message, agent_id);
        let (principal, workspace) = memory_service
            .scoped_memory_scope()
            .ok_or_else(|| anyhow!("agent_memory progress delivery requires scoped memory"))?;
        let mut data = memory_service
            .load_native_tier_by_name(
                agent_id,
                tier_name,
                &definition.definition.memory_tiers,
                Some(goal_id.as_str()),
            )
            .await
            .context("failed to load target memory tier")?
            .unwrap_or_else(|| {
                V3MemoryTierRecord::new(
                    tier_name.to_string(),
                    tier_definition.scope.clone(),
                    match tier_definition.scope {
                        TierScope::AgentGoal => Some(goal_id.as_str()),
                        TierScope::Agent | TierScope::User => None,
                    },
                    Some(principal),
                    Some(workspace),
                    if matches!(tier_definition.scope, TierScope::User) {
                        None
                    } else {
                        Some(agent_id)
                    },
                )
            });
        data.fields
            .insert("message_id".to_string(), json!(message.id));
        data.fields.insert("seq".to_string(), json!(message.seq));
        data.fields.insert(
            "severity".to_string(),
            json!(format!("{:?}", message.severity).to_lowercase()),
        );
        data.fields
            .insert("principal".to_string(), json!(message.principal));
        data.fields
            .insert("workspace".to_string(), json!(message.workspace));
        data.fields
            .insert("log_key".to_string(), json!(message.log_key));
        data.fields.insert(
            "source".to_string(),
            json!(format!("{:?}", message.source).to_lowercase()),
        );
        data.fields
            .insert("payload".to_string(), serde_json::to_value(message)?);
        data.last_updated = Self::message_time(message);
        memory_service
            .save_native_tier_by_name(
                agent_id,
                tier_name,
                &definition.definition.memory_tiers,
                match tier_definition.scope {
                    TierScope::AgentGoal => Some(goal_id.as_str()),
                    TierScope::Agent | TierScope::User => None,
                },
                &data,
            )
            .await
            .context("failed to save target memory tier")?;
        Ok(())
    }

    fn resolve_memory_service(
        &self,
        message: &ProgressMessage,
    ) -> anyhow::Result<AgentMemoryService> {
        self.memory_resolver
            .resolve_for_scope(&message.principal, &message.workspace)
            .context("agent_memory progress delivery requires explicit principal/workspace scope")
    }
}

#[async_trait]
impl ProgressChannel for AgentMemoryChannel {
    fn id(&self) -> &str {
        "agent_memory"
    }

    async fn deliver(
        &self,
        subscription: &Subscription,
        message: &ProgressMessage,
    ) -> anyhow::Result<()> {
        if message.source == ProgressSource::Projection {
            return Ok(());
        }
        // Taxonomy-driven visibility gate. Only terminal lifecycle
        // events + Hitl / Clarification events deserve an episode
        // entry. The decision lives in
        // `surface_routing::agent_memory_surface_renders_agent_event`,
        // sourced from the master `GAUI_EVENT_TAXONOMY` so adding a
        // new event_type with the right category routes here without
        // touching this file.
        if let Some(event_type) = message.event_type.as_deref() {
            if !agent_memory_surface_renders_agent_event(event_type) {
                return Ok(());
            }
        }
        let agent_id = subscription
            .metadata
            .get("agent_id")
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
            .or(message.agent_id.as_deref())
            .ok_or_else(|| anyhow!("agent_memory subscription missing agent_id metadata"))?;
        let tier_name = subscription
            .metadata
            .get("tier_name")
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("episode");
        let memory_service = self.resolve_memory_service(message)?;

        match tier_name {
            "episode" => {
                self.persist_episode(&memory_service, agent_id, message)
                    .await
            },
            _ => {
                self.persist_tier(&memory_service, agent_id, tier_name, message)
                    .await
            },
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use tempfile::tempdir;

    use crate::magician_v2::agents::{
        definition_store::AgentDefinitionStore, storage::AgentStorage, types::AgentKind,
        AgentDefinition, AgentMemoryResolver, NotificationRule, NotificationSeverity,
    };
    use crate::magician_v2::progress_channel_seam::types::{
        ProgressMessage, ProgressMessageKind, ProgressSeverity, ProgressSource, Subscription,
        SubscriptionFilter, SubscriptionSource,
    };

    use crate::magician_v2::progress_channel_seam::*;

    async fn seed_definition(store: &AgentDefinitionStore, agent_id: &str) {
        let mut definition = AgentDefinition {
            // Empty: every transport. The restriction is opt-in.
            browser_transports: Vec::new(),
            agent_id: agent_id.to_string(),
            version: 1,
            name: "Agent".to_string(),
            description: String::new(),
            app_tool: None,
            persona: "Persona".to_string(),
            kind: AgentKind::Personal,
            disabled: false,
            aliases: Vec::new(),
            wake_spellings: Vec::new(),
            tools: Vec::new(),
            excluded_tools: Vec::new(),
            denied_tools: Vec::new(),
            denied_tool_params: HashMap::new(),
            constraints: Default::default(),
            trust_level: Default::default(),
            memory_tiers: Vec::new(),
            memory_consolidation: Vec::new(),
            prompt_pipeline: None,
            circuit_breaker: None,
            feedback_loops: Vec::new(),
            notification_rules: vec![NotificationRule {
                r#match: "agent.cycle.failed".to_string(),
                severity: NotificationSeverity::Medium,
                channels: vec!["agent_memory".to_string()],
                condition: None,
                message: None,
            }],
            retention: None,
            llm_routing: None,
            strategy: None,
            state_machines: Default::default(),
            principal: None,
            workspace: None,
            autonomous_config: None,
            harness: None,
            is_primary: false,
            onboarding_completed: false,
            readable_agents: Vec::new(),
            default_personality: None,
            user_memory_isolation: Default::default(),
            delegation_targets: vec!["*".to_string()],
            invocation_policy: Default::default(),
            auto_surface_policy: None,
            chat_inline: None,
            social_persona: None,
        };
        definition.apply_defaults();
        store
            .create_definition(definition)
            .await
            .expect("definition");
    }

    fn sample_subscription() -> Subscription {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("agent_id".to_string(), "agent-a".to_string());
        metadata.insert("tier_name".to_string(), "episode".to_string());
        Subscription {
            id: "sub-1".to_string(),
            channel_id: "agent_memory".to_string(),
            filter: SubscriptionFilter::AgentId("agent-a".to_string()),
            principal: "principal-a".to_string(),
            workspace: "workspace-a".to_string(),
            min_severity: ProgressSeverity::Info,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: -1,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        }
    }

    fn terminal_message() -> ProgressMessage {
        ProgressMessage {
            id: "msg-1".to_string(),
            seq: 7,
            log_key: "task:task-1".to_string(),
            source: ProgressSource::Execution,
            event_type: None,
            metadata: Default::default(),
            execution_id: Some("exec-1".to_string()),
            task_id: Some("task-1".to_string()),
            root_task_id: Some("task-1".to_string()),
            root_execution_id: Some("exec-1".to_string()),
            parent_execution_id: None,
            agent_id: Some("agent-a".to_string()),
            ui_thread_id: None,
            step_id: None,
            routing_keys: vec!["task/task-1".to_string(), "agent/agent-a".to_string()],
            principal: "principal-a".to_string(),
            workspace: "workspace-a".to_string(),
            severity: ProgressSeverity::Info,
            kind: ProgressMessageKind::StatusChanged {
                status: "completed".to_string(),
                summary: Some("Finished cleanly".to_string()),
            },
            timestamp: 1_700_000_000_000,
        }
    }

    #[tokio::test]
    async fn agent_memory_channel_records_terminal_progress_as_episode() {
        let temp_dir = tempdir().expect("tempdir");
        let storage = AgentStorage::new(temp_dir.path());
        let definition_store = Arc::new(AgentDefinitionStore::new(storage.clone()));
        seed_definition(&definition_store, "agent-a").await;
        let memory_resolver = Arc::new(AgentMemoryResolver::new(temp_dir.path()));
        let channel = AgentMemoryChannel::new(memory_resolver.clone(), definition_store);

        channel
            .deliver(&sample_subscription(), &terminal_message())
            .await
            .expect("delivery should succeed");

        let episodes = memory_resolver
            .resolve_for_scope("principal-a", "workspace-a")
            .expect("scoped memory service")
            .load_native_episodes("agent-a")
            .await
            .expect("episodes");
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].episode_id, "progress-msg-1");
        assert_eq!(episodes[0].goal_id(), "task-1");
        assert!(episodes[0].outcome_is_succeeded());
    }

    #[tokio::test]
    async fn agent_memory_channel_ignores_projection_messages() {
        let temp_dir = tempdir().expect("tempdir");
        let storage = AgentStorage::new(temp_dir.path());
        let definition_store = Arc::new(AgentDefinitionStore::new(storage.clone()));
        seed_definition(&definition_store, "agent-a").await;
        let memory_resolver = Arc::new(AgentMemoryResolver::new(temp_dir.path()));
        let channel = AgentMemoryChannel::new(memory_resolver.clone(), definition_store);

        let mut projection_message = terminal_message();
        projection_message.source = ProgressSource::Projection;
        projection_message.kind = ProgressMessageKind::AgentNotification {
            event_type: "execution.progress".to_string(),
            message: "Projection summary".to_string(),
            entity_key: Some("execution:exec-1".to_string()),
            metadata: serde_json::json!({}),
        };

        channel
            .deliver(&sample_subscription(), &projection_message)
            .await
            .expect("delivery should succeed");

        let episodes = memory_resolver
            .resolve_for_scope("principal-a", "workspace-a")
            .expect("scoped memory service")
            .load_native_episodes("agent-a")
            .await
            .expect("episodes");
        assert!(episodes.is_empty());
    }
}
