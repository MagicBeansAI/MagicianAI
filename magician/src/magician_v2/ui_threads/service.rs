use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde_json::Value;
use tracing::warn;

use crate::magician_v2::{
    agents::{
        memory_tiers::{
            MemoryTierDefinition, RenderConfig, RetentionMode, TierFieldSchema, TierScope,
        },
        storage::sanitize_segment,
        AgentMemoryResolver,
    },
    artifact_v2::memory::V3MemoryTierRecord,
    artifact_v2::{ArtifactV2Service, ScopeRef, V3ReadApi},
    chat::storage::ChatStore,
    history::HistoryLane,
};

use super::{
    store::UiThreadStore,
    types::{
        UiThreadDetail, UiThreadPage, UiThreadRecord, UiThreadSearchCandidate, UiThreadUpdate,
    },
};

const THREAD_MEMORY_TIER_NAME: &str = "thread_context";
const THREAD_TASK_SCAN_LIMIT: usize = 500;
const SEARCH_SCOPE_SYNC_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Clone)]
pub struct UiThreadService {
    store: UiThreadStore,
    v3_service: Arc<ArtifactV2Service>,
    chat_store: Arc<dyn ChatStore>,
    memory_resolver: AgentMemoryResolver,
    search_sync_started_at: Arc<Mutex<HashMap<(String, String), Instant>>>,
}

impl UiThreadService {
    pub fn new(
        store: UiThreadStore,
        v3_service: Arc<ArtifactV2Service>,
        chat_store: Arc<dyn ChatStore>,
        memory_resolver: AgentMemoryResolver,
    ) -> Self {
        Self {
            store,
            v3_service,
            chat_store,
            memory_resolver,
            search_sync_started_at: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn resolve_memory_service(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<crate::magician_v2::agents::AgentMemoryService> {
        self.memory_resolver
            .resolve_for_scope(principal, workspace)
            .context("ui thread memory requires explicit principal/workspace scope")
    }

    pub async fn list_threads(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<UiThreadRecord>> {
        self.sync_scope(principal, workspace).await?;
        self.store.list_threads(principal, workspace).await
    }

    pub async fn list_threads_page(
        &self,
        principal: &str,
        workspace: &str,
        lane: Option<HistoryLane>,
        search: &str,
        limit: usize,
        offset: usize,
    ) -> Result<UiThreadPage> {
        self.sync_scope(principal, workspace).await?;
        self.store
            .list_threads_page(principal, workspace, lane, search, limit, offset)
            .await
    }

    pub async fn search_thread_candidates(
        &self,
        principal: &str,
        workspace: &str,
        search: &str,
    ) -> Result<Vec<UiThreadSearchCandidate>> {
        self.refresh_scope_in_background(principal, workspace);
        self.store
            .search_thread_candidates(principal, workspace, search)
            .await
    }

    fn refresh_scope_in_background(&self, principal: &str, workspace: &str) {
        let scope = (principal.to_string(), workspace.to_string());
        let should_start = {
            let mut started = self
                .search_sync_started_at
                .lock()
                .expect("ui thread search sync state mutex poisoned");
            if started
                .get(&scope)
                .is_some_and(|last| last.elapsed() < SEARCH_SCOPE_SYNC_INTERVAL)
            {
                false
            } else {
                started.insert(scope.clone(), Instant::now());
                true
            }
        };
        if !should_start {
            return;
        }

        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service.sync_scope(&scope.0, &scope.1).await {
                warn!(
                    principal = %scope.0,
                    workspace = %scope.1,
                    error = %error,
                    "background UI-thread scope refresh failed; serving materialized history"
                );
            }
        });
    }

    pub async fn get_materialized_thread(
        &self,
        principal: &str,
        workspace: &str,
        thread_id: &str,
    ) -> Result<Option<UiThreadRecord>> {
        self.store.get_thread(principal, workspace, thread_id).await
    }

    pub async fn get_thread(
        &self,
        principal: &str,
        workspace: &str,
        thread_id: &str,
    ) -> Result<Option<UiThreadDetail>> {
        self.sync_scope(principal, workspace).await?;
        let normalized_id = normalize_thread_id(thread_id)?;
        let Some(record) = self
            .store
            .get_thread(principal, workspace, &normalized_id)
            .await?
        else {
            return Ok(None);
        };
        let memory_text = self
            .load_thread_memory(principal, workspace, &normalized_id)
            .await?;
        Ok(Some(UiThreadDetail {
            record,
            memory_text,
        }))
    }

    pub async fn create_thread(
        &self,
        principal: &str,
        workspace: &str,
        thread_id: &str,
        name: Option<&str>,
    ) -> Result<UiThreadRecord> {
        let normalized_id = normalize_thread_id(thread_id)?;
        let normalized_name = normalize_thread_name(name.unwrap_or(thread_id))?;
        self.store
            .upsert_thread(principal, workspace, &normalized_id, &normalized_name)
            .await
    }

    pub async fn update_thread(
        &self,
        principal: &str,
        workspace: &str,
        thread_id: &str,
        name: Option<String>,
        archived: Option<bool>,
        memory_text: Option<Option<String>>,
        display_mode: Option<String>,
        plan_mode: Option<bool>,
    ) -> Result<Option<UiThreadDetail>> {
        let normalized_id = normalize_thread_id(thread_id)?;

        // Guard: #general cannot be archived or deleted.
        if normalized_id == "general" && archived == Some(true) {
            return Err(anyhow!("Cannot archive the #general thread"));
        }
        // Validate display_mode at the service boundary so the store
        // never sees a value outside the {chat, dev} contract.
        let normalized_display_mode = match display_mode.as_deref() {
            None => None,
            Some("chat") => Some("chat".to_string()),
            Some("dev") => Some("dev".to_string()),
            Some(other) => {
                return Err(anyhow!(
                    "display_mode must be 'chat' or 'dev', got '{}'",
                    other
                ));
            },
        };
        let memory_update = if let Some(memory_text) = memory_text {
            let saved = self
                .save_thread_memory(principal, workspace, &normalized_id, memory_text)
                .await?;
            Some(saved)
        } else {
            None
        };

        let record = self
            .store
            .update_thread(
                principal,
                workspace,
                &normalized_id,
                UiThreadUpdate {
                    name: name
                        .map(|value| normalize_thread_name(&value))
                        .transpose()?,
                    archived,
                    memory_summary: memory_update.as_ref().map(|(summary, _)| summary.clone()),
                    memory_updated_at: memory_update.as_ref().map(|(_, updated_at)| *updated_at),
                    display_mode: normalized_display_mode,
                    plan_mode,
                    ..Default::default()
                },
            )
            .await?;

        let Some(record) = record else {
            return Ok(None);
        };

        let memory_text = self
            .load_thread_memory(principal, workspace, &normalized_id)
            .await?;
        Ok(Some(UiThreadDetail {
            record,
            memory_text,
        }))
    }

    pub async fn delete_thread(
        &self,
        principal: &str,
        workspace: &str,
        thread_id: &str,
    ) -> Result<bool> {
        let normalized_id = normalize_thread_id(thread_id)?;
        if normalized_id == "general" {
            return Err(anyhow!("Cannot delete the #general thread"));
        }

        self.sync_scope(principal, workspace).await?;
        let deleted = self
            .store
            .delete_thread(principal, workspace, &normalized_id)
            .await?;
        if !deleted {
            return Ok(false);
        }

        for session in self
            .chat_store
            .list_sessions(principal, workspace)
            .await
            .with_context(|| {
                format!(
                    "listing chat sessions while deleting ui thread {principal}/{workspace}/{normalized_id}"
                )
            })?
        {
            if normalize_referenced_thread_id(&session.ui_thread_id).as_deref()
                == Some(normalized_id.as_str())
            {
                self.chat_store
                    .delete_session(&session.id)
                    .await
                    .with_context(|| {
                        format!(
                            "deleting chat session {} for ui thread {principal}/{workspace}/{normalized_id}",
                            session.id
                        )
                    })?;
            }
        }

        self.save_thread_memory(principal, workspace, &normalized_id, None)
            .await
            .with_context(|| {
                format!(
                    "clearing memory for deleted ui thread {principal}/{workspace}/{normalized_id}"
                )
            })?;

        Ok(true)
    }

    pub async fn reorder_threads(
        &self,
        principal: &str,
        workspace: &str,
        ordered_ids: Vec<String>,
    ) -> Result<Vec<UiThreadRecord>> {
        self.sync_scope(principal, workspace).await?;
        let mut normalized = Vec::new();
        for id in ordered_ids {
            let normalized_id = normalize_thread_id(&id)?;
            if !normalized.contains(&normalized_id) {
                normalized.push(normalized_id);
            }
        }
        self.store
            .reorder_threads(principal, workspace, &normalized)
            .await?;
        self.store.list_threads(principal, workspace).await
    }

    pub async fn sync_scope(&self, principal: &str, workspace: &str) -> Result<()> {
        let mut known_ids = HashMap::new();
        known_ids.insert("general".to_string(), HistoryLane::Personal);

        let scope = ScopeRef::system_internal_unauthenticated(
            &principal.to_string(),
            &workspace.to_string(),
        );
        let tasks = self.v3_service.list_tasks(&scope).await.with_context(|| {
            format!("listing tasks while syncing ui threads for scope {principal}/{workspace}")
        })?;
        for task in tasks.into_iter().take(THREAD_TASK_SCAN_LIMIT) {
            if let Some(thread_id) = normalize_referenced_thread_id(&task.ui_thread_id) {
                known_ids.entry(thread_id).or_insert(HistoryLane::Automated);
            }
        }

        // Two fields per session, so ask for two fields per session. `list_sessions`
        // took a session lock and read the whole session document for each one just
        // to reach `ui_thread_id` and the effective lane — 100+ locked reads on every
        // ui-threads list, page and detail. The store answers this from its in-memory
        // index instead, which already carries both.
        for (referenced_thread_id, lane) in self
            .chat_store
            .list_session_thread_lanes(principal, workspace)
            .await
            .with_context(|| {
                format!(
                    "listing chat sessions while syncing ui threads for scope {principal}/{workspace}"
                )
            })?
        {
            if let Some(thread_id) = normalize_referenced_thread_id(&referenced_thread_id) {
                known_ids
                    .entry(thread_id)
                    .and_modify(|current| {
                        if lane == HistoryLane::Personal {
                            *current = HistoryLane::Personal;
                        }
                    })
                    .or_insert(lane);
            }
        }

        let existing_ids: HashSet<String> = self
            .store
            .thread_ids_including_deleted(principal, workspace)
            .await?
            .into_iter()
            .collect();

        for (thread_id, history_lane) in known_ids {
            if existing_ids.contains(&thread_id) {
                continue;
            }
            let default_name = match thread_id.as_str() {
                "brainstorming" => "Brainstorming".to_string(),
                _ => thread_id.clone(),
            };
            self.store
                .upsert_thread_with_lane(
                    principal,
                    workspace,
                    &thread_id,
                    &default_name,
                    history_lane,
                )
                .await?;
        }
        Ok(())
    }

    async fn save_thread_memory(
        &self,
        principal: &str,
        workspace: &str,
        thread_id: &str,
        memory_text: Option<String>,
    ) -> Result<(Option<String>, Option<i64>)> {
        let agent_id = thread_memory_agent_id(principal, workspace, thread_id);
        let tier = thread_memory_tier_definition();

        let normalized_text = memory_text
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        match normalized_text {
            Some(text) => {
                let memory_service = self.resolve_memory_service(principal, workspace)?;
                let mut data = V3MemoryTierRecord::new(
                    THREAD_MEMORY_TIER_NAME.to_string(),
                    TierScope::Agent,
                    None,
                    Some(principal),
                    Some(workspace),
                    Some(&agent_id),
                );
                data.fields
                    .insert("text".to_string(), Value::String(text.clone()));
                if let Some(summary) = summarize_thread_memory(&text) {
                    data.fields
                        .insert("summary".to_string(), Value::String(summary.clone()));
                }
                data.last_updated = Utc::now();
                memory_service
                    .save_native_tier(&agent_id, &tier, None, &data)
                    .await
                    .context("saving thread memory tier")?;
                Ok((
                    summarize_thread_memory(&text),
                    Some(data.last_updated.timestamp_millis()),
                ))
            },
            None => {
                let memory_service = self.resolve_memory_service(principal, workspace)?;
                let mut data = V3MemoryTierRecord::new(
                    THREAD_MEMORY_TIER_NAME.to_string(),
                    TierScope::Agent,
                    None,
                    Some(principal),
                    Some(workspace),
                    Some(&agent_id),
                );
                data.fields.clear();
                data.last_updated = Utc::now();
                memory_service
                    .save_native_tier(&agent_id, &tier, None, &data)
                    .await
                    .context("clearing thread memory tier")?;
                Ok((None, None))
            },
        }
    }

    async fn load_thread_memory(
        &self,
        principal: &str,
        workspace: &str,
        thread_id: &str,
    ) -> Result<Option<String>> {
        let agent_id = thread_memory_agent_id(principal, workspace, thread_id);
        let tier = thread_memory_tier_definition();
        let data = self
            .resolve_memory_service(principal, workspace)?
            .load_native_tier(&agent_id, &tier, None)
            .await
            .context("loading thread memory tier")?;
        Ok(data
            .and_then(|tier| tier.fields.get("text").cloned())
            .and_then(|value| value.as_str().map(str::to_string))
            .filter(|value| !value.trim().is_empty()))
    }
}

fn thread_memory_agent_id(principal: &str, workspace: &str, thread_id: &str) -> String {
    format!(
        "ui-thread--{}--{}--{}",
        sanitize_segment(principal),
        sanitize_segment(workspace),
        sanitize_segment(thread_id)
    )
}

fn thread_memory_tier_definition() -> MemoryTierDefinition {
    let mut schema = std::collections::BTreeMap::new();
    schema.insert("text".to_string(), TierFieldSchema::Document {});
    schema.insert("summary".to_string(), TierFieldSchema::Text {});
    MemoryTierDefinition {
        name: THREAD_MEMORY_TIER_NAME.to_string(),
        scope: TierScope::Agent,
        description: "Persisted thread-scoped UI context".to_string(),
        schema,
        render: RenderConfig {
            format: "plain_text".to_string(),
            template: "{summary}".to_string(),
        },
        retention: RetentionMode::Forever,
    }
}

fn summarize_thread_memory(text: &str) -> Option<String> {
    let normalized = text.trim().replace('\n', " ");
    if normalized.is_empty() {
        return None;
    }
    let summary: String = normalized.chars().take(240).collect();
    Some(summary)
}

fn normalize_thread_name(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("thread name must not be empty"));
    }
    Ok(trimmed.to_string())
}

// Implement ThreadArchivedChecker for UiThreadService so the chat service
// can check thread status without a hard dependency on the full service.
#[async_trait::async_trait]
impl crate::magician_v2::chat::service::ThreadArchivedChecker for UiThreadService {
    async fn is_thread_archived(&self, principal: &str, workspace: &str, thread_id: &str) -> bool {
        match self.store.get_thread(principal, workspace, thread_id).await {
            Ok(Some(record)) => record.archived,
            _ => false, // unknown threads are not considered archived
        }
    }
}

pub fn normalize_thread_id(value: &str) -> Result<String> {
    let normalized = value.trim().to_lowercase();
    if normalized.is_empty() {
        return Err(anyhow!("thread id must not be empty"));
    }
    if !normalized
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(anyhow!(
            "thread id must contain only lowercase letters, numbers, or hyphens"
        ));
    }
    Ok(normalized)
}

fn normalize_referenced_thread_id(value: &str) -> Option<String> {
    normalize_thread_id(value)
        .ok()
        .or_else(|| slugify_legacy_thread_id(value))
}

fn slugify_legacy_thread_id(value: &str) -> Option<String> {
    let mut output = String::new();
    let mut last_was_dash = false;
    for ch in value.trim().to_lowercase().chars() {
        let next = if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            ch
        } else {
            '-'
        };
        if next == '-' {
            if output.is_empty() || last_was_dash {
                continue;
            }
            last_was_dash = true;
        } else {
            last_was_dash = false;
        }
        output.push(next);
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        None
    } else {
        Some(output)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use crate::magician_v2::{
        chat::{
            models::ChatChannel,
            storage::{ChatStore, FileChatStore},
        },
        test_support::build_test_artifact_v2_service,
    };

    use super::*;

    #[test]
    fn referenced_thread_ids_slug_legacy_colon_ids() {
        assert_eq!(
            normalize_referenced_thread_id("contextual-writing:site-http-localhost-5173")
                .as_deref(),
            Some("contextual-writing-site-http-localhost-5173")
        );
        assert_eq!(
            normalize_referenced_thread_id("  screen-watch  ").as_deref(),
            Some("screen-watch")
        );
        assert_eq!(normalize_referenced_thread_id("   "), None);
    }

    #[tokio::test]
    async fn sync_scope_backfills_threads_from_tasks_and_chat() {
        let tmp = TempDir::new().unwrap();
        let v3_service = build_test_artifact_v2_service(tmp.path());
        let chat_store = Arc::new(
            FileChatStore::with_index(tmp.path().join("chat"))
                .await
                .unwrap(),
        );
        let memory_resolver = AgentMemoryResolver::new(tmp.path());
        let store = UiThreadStore::open(tmp.path()).unwrap();
        let service = UiThreadService::new(store, v3_service, chat_store.clone(), memory_resolver);
        let _session = chat_store
            .get_or_create_active_session(
                "alpha",
                "prod",
                "travel",
                &ChatChannel {
                    channel_type: "web".to_string(),
                    address: None,
                },
                "personal-assistant",
            )
            .await
            .unwrap();
        let threads = service.list_threads("alpha", "prod").await.unwrap();
        assert!(threads.iter().any(|thread| thread.id == "general"));
        assert!(threads.iter().any(|thread| thread.id == "travel"));
    }

    #[tokio::test]
    async fn sync_scope_does_not_overwrite_existing_thread_name() {
        let tmp = TempDir::new().unwrap();
        let v3_service = build_test_artifact_v2_service(tmp.path());
        let chat_store = Arc::new(
            FileChatStore::with_index(tmp.path().join("chat"))
                .await
                .unwrap(),
        );
        let memory_resolver = AgentMemoryResolver::new(tmp.path());
        let store = UiThreadStore::open(tmp.path()).unwrap();
        store
            .upsert_thread("alpha", "prod", "travel", "Travel Planning")
            .await
            .unwrap();
        let service = UiThreadService::new(store.clone(), v3_service, chat_store, memory_resolver);

        service.sync_scope("alpha", "prod").await.unwrap();

        let thread = store
            .get_thread("alpha", "prod", "travel")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(thread.name, "Travel Planning");
    }

    #[tokio::test]
    async fn delete_thread_removes_chat_sessions_and_prevents_resync_recreation() {
        let tmp = TempDir::new().unwrap();
        let v3_service = build_test_artifact_v2_service(tmp.path());
        let chat_store = Arc::new(
            FileChatStore::with_index(tmp.path().join("chat"))
                .await
                .unwrap(),
        );
        let memory_resolver = AgentMemoryResolver::new(tmp.path());
        let store = UiThreadStore::open(tmp.path()).unwrap();
        let service = UiThreadService::new(
            store.clone(),
            v3_service,
            chat_store.clone(),
            memory_resolver,
        );
        let session = chat_store
            .get_or_create_active_session(
                "alpha",
                "prod",
                "travel",
                &ChatChannel {
                    channel_type: "web".to_string(),
                    address: None,
                },
                "personal-assistant",
            )
            .await
            .unwrap();

        assert!(service
            .delete_thread("alpha", "prod", "travel")
            .await
            .unwrap());
        assert!(chat_store.get_session(&session.id).await.unwrap().is_none());

        service.sync_scope("alpha", "prod").await.unwrap();
        let threads = service.list_threads("alpha", "prod").await.unwrap();
        assert!(threads.iter().any(|thread| thread.id == "general"));
        assert!(!threads.iter().any(|thread| thread.id == "travel"));
    }

    #[tokio::test]
    async fn delete_thread_rejects_general() {
        let tmp = TempDir::new().unwrap();
        let v3_service = build_test_artifact_v2_service(tmp.path());
        let chat_store = Arc::new(
            FileChatStore::with_index(tmp.path().join("chat"))
                .await
                .unwrap(),
        );
        let memory_resolver = AgentMemoryResolver::new(tmp.path());
        let store = UiThreadStore::open(tmp.path()).unwrap();
        let service = UiThreadService::new(store, v3_service, chat_store, memory_resolver);

        assert!(service
            .delete_thread("alpha", "prod", "general")
            .await
            .is_err());
    }
}
