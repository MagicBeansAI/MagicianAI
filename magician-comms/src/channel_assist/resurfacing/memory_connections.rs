//! Connect canonical memory with attention items; reconcile durable HITL answers.
use super::interaction::{ResurfacingInteractionRegistry, ResurfacingSourceStatus};
use anyhow::{ensure, Result};
use magician::magician_v2::{
    agents::memory_prompt_blocks::user_memory_sources_are_current,
    agents::AgentMemoryResolver,
    attention::resurfacing::{
        memory_connections::*,
        store::ResurfacingStore,
        types::{Candidate, CandidateState, FeedbackAction},
    },
    attention_funnel::*,
    attention_funnel_store::AttentionFunnelStore,
    chat::service::merge_user_memory_tier_fields,
    feed::{FeedItem, FeedItemStatus, FeedItemType, FeedStore},
    query_analysis::operation_llm_router::{OperationLlmRouter, SimplifiedLLMResponse},
    taste_profile::TasteProfileLoader,
    user_requests::{
        RequestOption, ScopedResponseResult, UserRequest, UserRequestService, UserResponse,
    },
};
use serde_json::json;
use std::{sync::Arc, time::Duration};

#[derive(Clone)]
pub struct ConnectionRuntime {
    pub store: ResurfacingStore,
    pub resolver: AgentMemoryResolver,
    pub interactions: ResurfacingInteractionRegistry,
    pub feed: FeedStore,
    pub requests: Option<Arc<UserRequestService>>,
    pub router: Option<Arc<OperationLlmRouter>>,
    pub attention: Option<AttentionFunnelStore>,
    pub taste: Option<Arc<TasteProfileLoader>>,
}

/// Optional observation of the real review boundary for focused evaluations.
/// The observer cannot replace inputs, model results, validation, or delivery.
pub struct ConnectionReviewObservation {
    pub candidate_id: String,
    pub sources: Vec<ConnectionSource>,
    pub response: Option<SimplifiedLLMResponse>,
    pub error: Option<String>,
    pub elapsed_ms: u64,
}

fn source_content_digest(sources: &[ConnectionSource]) -> String {
    let mut digest = blake3::Hasher::new();
    digest.update(&(sources.len() as u64).to_le_bytes());
    for source in sources {
        for field in [&source.id, &source.key, &source.revision, &source.text] {
            digest.update(&(field.len() as u64).to_le_bytes());
            digest.update(field.as_bytes());
        }
    }
    digest.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::super::interaction::{
        ResolvedResurfacingDetail, ResurfacingActionCapability, ResurfacingInteractionAdapter,
    };
    use super::*;
    use magician::magician_v2::attention::resurfacing::types::SourceKind;
    use magician::magician_v2::{
        artifact_v2::workspace::ArtifactV2Workspace, realtime_events::RuntimeTransportBroadcaster,
    };

    #[test]
    fn observation_source_identity_includes_text_and_order() {
        let first = ConnectionSource {
            id: "activity".into(),
            key: "task-1".into(),
            revision: "v1".into(),
            text: "Meeting is tomorrow".into(),
        };
        let mut changed = first.clone();
        changed.text = "Meeting was cancelled".into();
        assert_ne!(
            source_content_digest(&[first.clone()]),
            source_content_digest(&[changed])
        );
        assert_ne!(
            source_content_digest(&[first.clone(), first.clone()]),
            source_content_digest(&[first.clone()])
        );
        let mut other = first.clone();
        other.id = "m0".into();
        assert_ne!(
            source_content_digest(&[first.clone(), other.clone()]),
            source_content_digest(&[other, first])
        );
    }

    struct Available;
    #[async_trait::async_trait]
    impl ResurfacingInteractionAdapter for Available {
        fn source_kind(&self) -> SourceKind {
            SourceKind::Task
        }
        async fn resolve_detail(
            &self,
            _scope: &AttentionScope,
            c: &Candidate,
        ) -> Result<ResolvedResurfacingDetail> {
            Ok(ResolvedResurfacingDetail {
                status: ResurfacingSourceStatus::Available,
                title: Some(c.title.clone()),
                summary: Some(c.content_digest.clone()),
                source_revision: c.content_revision.clone(),
                source_updated: false,
                has_newer: false,
                source_route: Some("/tasks".into()),
                open_url: None,
                source: None,
                original: None,
                actions: vec![],
            })
        }
        fn capabilities(
            &self,
            _candidate: &Candidate,
            _detail: Option<&ResolvedResurfacingDetail>,
        ) -> Vec<ResurfacingActionCapability> {
            vec![]
        }
    }

    async fn fixture(
        surface: ConnectionSurface,
    ) -> (
        tempfile::TempDir,
        ConnectionRuntime,
        AttentionScope,
        ConnectionRecord,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let scope = AttentionScope {
            principal: "p".into(),
            workspace: "w".into(),
        };
        let workspace = ArtifactV2Workspace::new(temp.path());
        let resolver = AgentMemoryResolver::with_workspace_layout(workspace.clone());
        resolver.resolve_for_scope("p","w").unwrap().persist_user_knowledge(&json!({"user_preferences":[{"key":"conference","value":"I want to meet more climate founders","source_type":"explicit_user_statement","confidence":1.0}]})).await.unwrap();
        let requests = UserRequestService::new(Arc::new(RuntimeTransportBroadcaster::new(64)))
            .with_workspace_layout(workspace.clone())
            .with_history_persist_path(temp.path().join("request-history.json"))
            .with_pending_persist_path(temp.path().join("request-pending.json"))
            .await;
        let runtime = ConnectionRuntime {
            store: ResurfacingStore::open(temp.path()).unwrap(),
            resolver,
            interactions: ResurfacingInteractionRegistry::from_adapters(vec![Arc::new(Available)]),
            feed: FeedStore::open_workspace(workspace).unwrap(),
            requests: Some(Arc::new(requests)),
            router: None,
            attention: Some(AttentionFunnelStore::open(temp.path()).unwrap()),
            taste: None,
        };
        let now = chrono::Utc::now().timestamp();
        let candidate = Candidate {
            candidate_id: "conference-task".into(),
            source_kind: SourceKind::Task,
            source_ref: "task-1".into(),
            title: "Climate conference".into(),
            content_digest: "Conference early booking closes Friday".into(),
            content_revision: Some("r1".into()),
            content_details: None,
            semantic_features: None,
            salience_score: 0.7,
            signals: Default::default(),
            temporal_anchor_at: None,
            embedding_id: None,
            state: CandidateState::Candidate,
            first_seen_at: now,
            last_scored_at: now,
            last_surfaced_at: None,
            cooldown_until: 0,
            surface_count: 0,
            dismiss_count: 0,
        };
        runtime
            .store
            .upsert_candidate("p", "w", &candidate)
            .await
            .unwrap();
        let (sources, source_route) = runtime
            .current_sources(&scope, &candidate)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sources.len(), 2);
        let connection = Connection {
            surface,
            confidence: 0.95,
            title: "Climate founder connections".into(),
            summary: "This conference may support your stated networking goal.".into(),
            question: if surface == ConnectionSurface::Hitl {
                Some("Is meeting climate founders still relevant for this conference?".into())
            } else {
                None
            },
            evidence: vec![
                ConnectionCitation {
                    id: "activity".into(),
                    quote: "Conference early booking closes Friday".into(),
                },
                ConnectionCitation {
                    id: "m0".into(),
                    quote: "I want to meet more climate founders".into(),
                },
            ],
        };
        let record = ConnectionRecord {
            candidate_id: candidate.candidate_id,
            fingerprint: fingerprint(&sources),
            attempted_at: now,
            state: ConnectionState::Ready,
            sources,
            connection: Some(connection),
            request_id: "test-memory-connection".into(),
            feed_id: "memory_connection:test".into(),
            source_route,
            decision_policy: None,
            decision_origin: None,
        };
        runtime
            .store
            .put_connection("p", "w", &record)
            .await
            .unwrap();
        (temp, runtime, scope, record)
    }

    #[tokio::test]
    async fn memory_connections_policy_outage_defers_publication_but_still_cleans_revoked_sources()
    {
        use magician::{
            config::{DecisionHostConfig, DecisionMode},
            magician_v2::decision_host,
        };
        let (_temp, mut runtime, scope, mut record) = fixture(ConnectionSurface::ForYou).await;
        runtime.router = Some(Arc::new(OperationLlmRouter::new(None)));
        record.decision_policy = Some("saved-qualified-policy".into());
        runtime
            .store
            .put_connection("p", "w", &record)
            .await
            .unwrap();
        decision_host::configure(&DecisionHostConfig {
            mode: DecisionMode::AllEngines,
            socket: Some(format!(
                "/tmp/missing-memory-policy-{}.sock",
                std::process::id()
            )),
            ..Default::default()
        });
        let now = record.attempted_at;
        runtime.reconcile(&scope, &mut record, now).await.unwrap();
        assert_eq!(record.state, ConnectionState::Ready);
        assert!(runtime
            .feed
            .get_item("p", "w", &record.feed_id)
            .await
            .unwrap()
            .is_none());
        let memory = runtime.resolver.resolve_for_scope("p", "w").unwrap();
        let mut knowledge = memory.load_user_knowledge().await.unwrap();
        knowledge["user_preferences"][0]["memory_lifecycle"] = json!("superseded");
        memory.persist_user_knowledge(&knowledge).await.unwrap();
        runtime
            .reconcile(&scope, &mut record, now + 1)
            .await
            .unwrap();
        assert_eq!(record.state, ConnectionState::Withdrawn);
        decision_host::configure(&DecisionHostConfig {
            mode: DecisionMode::Off,
            ..Default::default()
        });
    }

    #[tokio::test]
    async fn memory_connections_existing_surfaces_and_dismissal_are_durable_and_scoped() {
        for surface in [ConnectionSurface::WorthALook, ConnectionSurface::ForYou] {
            let (_temp, runtime, scope, mut record) = fixture(surface).await;
            let now = record.attempted_at;
            runtime.reconcile(&scope, &mut record, now).await.unwrap();
            assert_eq!(record.state, ConnectionState::Published);
            assert!(runtime
                .store
                .get_connection("other", "w", &record.candidate_id)
                .await
                .unwrap()
                .is_none());
            let candidate = runtime
                .store
                .get_candidate("p", "w", &record.candidate_id)
                .await
                .unwrap()
                .unwrap();
            if surface == ConnectionSurface::WorthALook {
                assert_eq!(candidate.state, CandidateState::Surfaced);
                let (_, why) = runtime
                    .store
                    .get_phrasing("p", "w", &record.candidate_id)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(why.contains("climate founders"));
            } else {
                assert_eq!(candidate.state, CandidateState::Candidate);
                assert!(candidate.cooldown_until > now);
                assert!(runtime
                    .feed
                    .get_item("p", "w", &record.feed_id)
                    .await
                    .unwrap()
                    .is_some());
            }
            runtime
                .store
                .record_action(
                    "p",
                    "w",
                    &record.candidate_id,
                    FeedbackAction::Dismiss,
                    now + 1,
                    100,
                    1000,
                )
                .await
                .unwrap();
            let mut restored = runtime
                .store
                .get_connection("p", "w", &record.candidate_id)
                .await
                .unwrap()
                .unwrap();
            runtime
                .reconcile(&scope, &mut restored, now + 2)
                .await
                .unwrap();
            assert_eq!(restored.state, ConnectionState::Withdrawn);
            assert!(runtime
                .feed
                .get_item("p", "w", &record.feed_id)
                .await
                .unwrap()
                .is_none());
            assert!(runtime
                .store
                .get_phrasing("p", "w", &record.candidate_id)
                .await
                .unwrap()
                .is_none());
        }
    }

    #[tokio::test]
    async fn memory_connections_survive_late_curation_but_not_changed_activity() {
        let (_temp, runtime, scope, mut record) = fixture(ConnectionSurface::WorthALook).await;
        let now = record.attempted_at;
        runtime.reconcile(&scope, &mut record, now).await.unwrap();
        runtime
            .store
            .upsert_phrasing(
                "p",
                "w",
                &record.candidate_id,
                "Ordinary curation",
                "Original context",
                Some("r1"),
                now + 1,
            )
            .await
            .unwrap();
        let single = runtime
            .store
            .get_phrasing("p", "w", &record.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert!(single.1.contains("climate founders"));
        let batch = runtime
            .store
            .get_phrasing_batch("p", "w", std::slice::from_ref(&record.candidate_id))
            .await
            .unwrap();
        assert_eq!(batch[&record.candidate_id], single);
        let mut candidate = runtime
            .store
            .get_candidate("p", "w", &record.candidate_id)
            .await
            .unwrap()
            .unwrap();
        candidate.content_digest = "The conference was cancelled".into();
        runtime
            .store
            .upsert_candidate("p", "w", &candidate)
            .await
            .unwrap();
        let (_, why) = runtime
            .store
            .get_phrasing("p", "w", &record.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert!(!why.contains("climate founders"));
        runtime
            .reconcile(&scope, &mut record, now + 2)
            .await
            .unwrap();
        assert_eq!(record.state, ConnectionState::Withdrawn);
    }

    #[tokio::test]
    async fn memory_connections_owner_clarification_survives_consumer_restart_without_promoting_inference(
    ) {
        let (_temp, runtime, scope, mut record) = fixture(ConnectionSurface::Hitl).await;
        let now = record.attempted_at;
        runtime.reconcile(&scope, &mut record, now).await.unwrap();
        let requests = runtime.requests.as_ref().unwrap();
        let answer = UserResponse {
            request_id: record.request_id.clone(),
            decision: "remember".into(),
            input: Some("For this conference, focus on local founders.".into()),
            channel: "web".into(),
            sensitive: Vec::new(),
        };
        assert_eq!(
            requests
                .respond_scoped(answer.clone(), Some("other"), Some("w"))
                .await,
            ScopedResponseResult::ScopeMismatch
        );
        assert_eq!(
            requests.respond_scoped(answer, Some("p"), Some("w")).await,
            ScopedResponseResult::Accepted
        );
        let mut restored = runtime
            .store
            .get_connection("p", "w", &record.candidate_id)
            .await
            .unwrap()
            .unwrap();
        runtime
            .reconcile(&scope, &mut restored, now + 1)
            .await
            .unwrap();
        assert_eq!(restored.state, ConnectionState::Done);
        let knowledge = runtime
            .resolver
            .resolve_for_scope("p", "w")
            .unwrap()
            .load_user_knowledge()
            .await
            .unwrap();
        let preferences = knowledge["user_preferences"].as_array().unwrap();
        assert_eq!(preferences.len(), 2);
        assert!(preferences.iter().any(|p| p["value"]
            == "For this conference, focus on local founders."
            && p["source_type"] == "explicit_user_statement"));
        assert!(!preferences
            .iter()
            .any(|p| p["value"] == "This conference may support your stated networking goal."));
        assert_eq!(
            runtime
                .store
                .connection_calls_since("p", "w", now - 3600)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn memory_connections_generate_through_scoped_router_and_reject_forged_evidence() {
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};
        for forged in [false, true] {
            let (_temp, mut runtime, scope, mut seed) = fixture(ConnectionSurface::ForYou).await;
            let now = seed.attempted_at;
            let mut answer = seed.connection.take().unwrap();
            if forged {
                answer.evidence[1].quote = "An invented owner preference".into();
            }
            // Use a new candidate identity so the fixture's ready decision cannot
            // bypass generation or overwrite the monotonic generation guard.
            let mut candidate = runtime
                .store
                .get_candidate("p", "w", &seed.candidate_id)
                .await
                .unwrap()
                .unwrap();
            candidate.candidate_id = "generated-task".into();
            runtime
                .store
                .upsert_candidate("p", "w", &candidate)
                .await
                .unwrap();
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "model":"connection-test", "done":true,
                    "response":serde_json::to_string(&json!({"connection":answer})).unwrap()
                })))
                .expect(1)
                .mount(&server)
                .await;
            let config = serde_json::from_value(json!({
                "profiles":{"test":{"provider":"ollama","model":"connection-test","api_base_url":format!("{}/api/generate",server.uri())}},
                "default_profile":"test", "operation_mapping":{(OPERATION):"test"}
            })).unwrap();
            runtime.router = Some(Arc::new(OperationLlmRouter::new(Some(config))));
            let mut cursor = Some("conference-task".into());
            assert_eq!(runtime.pass(&scope, &mut cursor, now).await.unwrap(), 1);
            let generated = runtime
                .store
                .get_connection("p", "w", "generated-task")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                generated.state,
                if forged {
                    ConnectionState::Failed
                } else {
                    ConnectionState::Published
                }
            );
            assert_eq!(
                runtime
                    .feed
                    .get_item("p", "w", &generated.feed_id)
                    .await
                    .unwrap()
                    .is_some(),
                !forged
            );
            let requests = server.received_requests().await.unwrap();
            let payload = String::from_utf8(requests[0].body.clone()).unwrap();
            assert!(payload.contains("Conference early booking closes Friday"));
            assert!(payload.contains("I want to meet more climate founders"));
            assert!(runtime
                .store
                .get_connection("other", "w", "generated-task")
                .await
                .unwrap()
                .is_none());
        }
    }

    #[tokio::test]
    async fn memory_connections_citations_require_current_scoped_canonical_memory() {
        let (_temp, runtime, _scope, record) = fixture(ConnectionSurface::WorthALook).await;
        let citations: Vec<_> = record
            .sources
            .iter()
            .filter(|s| s.id != "activity")
            .map(|s| (s.key.clone(), s.revision.clone()))
            .collect();
        let own = runtime.resolver.resolve_for_scope("p", "w").unwrap();
        assert!(user_memory_sources_are_current(&own, &citations)
            .await
            .unwrap());
        let other = runtime.resolver.resolve_for_scope("other", "w").unwrap();
        other.persist_user_knowledge(&json!({"user_preferences":[{"key":"conference","value":"I prefer to skip conferences"}]})).await.unwrap();
        assert!(!user_memory_sources_are_current(&other, &citations)
            .await
            .unwrap());
        let mut knowledge = own.load_user_knowledge().await.unwrap();
        knowledge["user_preferences"][0]["memory_lifecycle"] = json!("superseded");
        own.persist_user_knowledge(&knowledge).await.unwrap();
        assert!(!user_memory_sources_are_current(&own, &citations)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn memory_connections_replayed_feedback_is_idempotent() {
        let (_temp, runtime, scope, record) = fixture(ConnectionSurface::Hitl).await;
        for now in [record.attempted_at, record.attempted_at + 1] {
            runtime
                .record_feedback(&scope, &record, FeedbackAction::Dismiss, now, "answer")
                .await
                .unwrap();
        }
        let candidate = runtime
            .store
            .get_candidate("p", "w", &record.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(candidate.dismiss_count, 1);
        let repairs = runtime
            .store
            .list_feedback_attention_repairs_after("p", "w", 0, 0, 10)
            .await
            .unwrap();
        assert_eq!(repairs.len(), 1);
        assert!(repairs[0].event_id.ends_with(":answer"));
    }

    #[tokio::test]
    async fn memory_connections_stale_answer_cannot_write_owner_memory() {
        let (_temp, runtime, scope, mut record) = fixture(ConnectionSurface::Hitl).await;
        let now = record.attempted_at;
        runtime.reconcile(&scope, &mut record, now).await.unwrap();
        runtime
            .requests
            .as_ref()
            .unwrap()
            .respond_scoped(
                UserResponse {
                    request_id: record.request_id.clone(),
                    decision: "remember".into(),
                    input: Some("A stale clarification".into()),
                    channel: "web".into(),
                    sensitive: Vec::new(),
                },
                Some("p"),
                Some("w"),
            )
            .await;
        runtime
            .resolver
            .resolve_for_scope("p", "w")
            .unwrap()
            .persist_user_knowledge(&json!({"user_preferences":[]}))
            .await
            .unwrap();
        runtime
            .reconcile(&scope, &mut record, now + 1)
            .await
            .unwrap();
        assert_eq!(record.state, ConnectionState::Withdrawn);
        assert_eq!(
            runtime
                .resolver
                .resolve_for_scope("p", "w")
                .unwrap()
                .load_user_knowledge()
                .await
                .unwrap()["user_preferences"],
            json!([])
        );
    }

    async fn write_taste(
        loader: &TasteProfileLoader,
        principal: &str,
        workspace: &str,
        text: &str,
    ) {
        loader
            .notes_store()
            .write_note_markdown(
                principal,
                workspace,
                magician::magician_v2::notes::WriteNoteMarkdownRequest {
                    provider: Some("local_markdown".into()),
                    target_path: "profile.md".into(),
                    markdown: text.into(),
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn memory_connections_profile_recall_is_scoped_and_never_clips_exceptions() {
        use magician::magician_v2::{
            notes::NotesSettingsStore, taste_profile::TasteProfileSettings,
        };
        let (temp, mut runtime, scope, _) = fixture(ConnectionSurface::WorthALook).await;
        let loader = Arc::new(TasteProfileLoader::new(
            NotesSettingsStore::with_workspace_layout(ArtifactV2Workspace::new(temp.path())),
            TasteProfileSettings::default(),
        ));
        runtime.taste = Some(loader.clone());
        assert!(runtime.profile_source(&scope).await.is_none());
        write_taste(
            &loader,
            "other",
            "w",
            "FOREIGN CANARY: I prefer indoor meetings.",
        )
        .await;
        assert!(runtime.profile_source(&scope).await.is_none());
        write_taste(
            &loader,
            "p",
            "w",
            "I want to meet climate founders in person.",
        )
        .await;
        let source = runtime.profile_source(&scope).await.unwrap();
        assert_eq!(source.id, "owner_profile");
        assert!(!source.text.contains("CANARY"));
        assert!(runtime
            .profile_source(&AttentionScope {
                principal: "p".into(),
                workspace: "elsewhere".into()
            })
            .await
            .is_none());
        write_taste(
            &loader,
            "p",
            "w",
            &format!("{}\nException: never flag this.", "x".repeat(4000)),
        )
        .await;
        assert!(runtime.profile_source(&scope).await.is_none());
        runtime.taste = None;
        assert!(runtime.profile_source(&scope).await.is_none());
    }

    #[tokio::test]
    async fn memory_connections_profile_revision_withdraws_before_owner_clarification_write() {
        use magician::magician_v2::{
            notes::NotesSettingsStore, taste_profile::TasteProfileSettings,
        };
        let (temp, mut runtime, scope, mut record) = fixture(ConnectionSurface::Hitl).await;
        let loader = Arc::new(TasteProfileLoader::new(
            NotesSettingsStore::with_workspace_layout(ArtifactV2Workspace::new(temp.path())),
            TasteProfileSettings::default(),
        ));
        runtime.taste = Some(loader.clone());
        write_taste(&loader, "p", "w", "I want to meet more climate founders").await;
        let source = runtime.profile_source(&scope).await.unwrap();
        record.sources.retain(|s| s.id == "activity");
        record.sources.push(source);
        record.connection.as_mut().unwrap().evidence[1].id = "owner_profile".into();
        record.fingerprint = fingerprint(&record.sources);
        runtime
            .store
            .put_connection("p", "w", &record)
            .await
            .unwrap();
        let now = record.attempted_at;
        runtime.reconcile(&scope, &mut record, now).await.unwrap();
        assert_eq!(record.state, ConnectionState::Published);
        let before = runtime
            .resolver
            .resolve_for_scope("p", "w")
            .unwrap()
            .load_user_knowledge()
            .await
            .unwrap();
        assert_eq!(
            runtime
                .requests
                .as_ref()
                .unwrap()
                .respond_scoped(
                    UserResponse {
                        request_id: record.request_id.clone(),
                        decision: "remember".into(),
                        input: Some("An outdated clarification".into()),
                        channel: "web".into(),
                        sensitive: Vec::new(),
                    },
                    Some("p"),
                    Some("w")
                )
                .await,
            ScopedResponseResult::Accepted
        );
        write_taste(
            &loader,
            "p",
            "w",
            "I no longer want introductions at this conference.",
        )
        .await;
        runtime
            .reconcile(&scope, &mut record, now + 1)
            .await
            .unwrap();
        assert_eq!(record.state, ConnectionState::Withdrawn);
        assert_eq!(
            runtime
                .resolver
                .resolve_for_scope("p", "w")
                .unwrap()
                .load_user_knowledge()
                .await
                .unwrap(),
            before
        );
    }
}

impl ConnectionRuntime {
    /// Reconciliation is independent of model availability and its hourly budget.
    /// The cursor only schedules work; all decisions/budget/answers are durable.
    pub async fn pass(
        &self,
        scope: &AttentionScope,
        cursor: &mut Option<String>,
        now: i64,
    ) -> Result<usize> {
        self.pass_inner(scope, cursor, now, None).await
    }

    pub async fn pass_with_observer(
        &self,
        scope: &AttentionScope,
        cursor: &mut Option<String>,
        now: i64,
        observer: &mut (dyn FnMut(ConnectionReviewObservation) + Send),
    ) -> Result<usize> {
        self.pass_inner(scope, cursor, now, Some(observer)).await
    }

    async fn pass_inner(
        &self,
        scope: &AttentionScope,
        cursor: &mut Option<String>,
        now: i64,
        mut observer: Option<&mut (dyn FnMut(ConnectionReviewObservation) + Send)>,
    ) -> Result<usize> {
        let active = self
            .store
            .active_connections(&scope.principal, &scope.workspace)
            .await?;
        for mut record in active.iter().cloned() {
            if let Err(error) = self.reconcile(scope, &mut record, now).await {
                tracing::warn!(%error, "memory connection reconciliation deferred");
            }
        }
        if active.len() >= 100 {
            return Ok(0);
        }
        let Some(router) = self
            .router
            .as_ref()
            .filter(|r| r.explicit_binding_for_operation(OPERATION).is_some())
        else {
            return Ok(0);
        };
        let spent = self
            .store
            .connection_calls_since(&scope.principal, &scope.workspace, now - 3600)
            .await?;
        if spent >= MAX_CALLS_PER_HOUR {
            return Ok(0);
        }
        let candidates = self
            .store
            .list_active_semantic_candidates(
                &scope.principal,
                &scope.workspace,
                cursor.as_deref(),
                20,
            )
            .await?;
        if candidates.is_empty() {
            *cursor = None;
            return Ok(0);
        }
        let mut calls = 0;
        for candidate in candidates {
            if calls + spent >= MAX_CALLS_PER_HOUR {
                break;
            }
            *cursor = Some(candidate.candidate_id.clone());
            if candidate.cooldown_until > now {
                continue;
            }
            let old = self
                .store
                .get_connection(&scope.principal, &scope.workspace, &candidate.candidate_id)
                .await?;
            if old.as_ref().is_some_and(|r| {
                matches!(
                    r.state,
                    ConnectionState::Ready
                        | ConnectionState::Published
                        | ConnectionState::Done
                        | ConnectionState::Withdrawing
                ) || now - r.attempted_at < 3600
            }) {
                continue;
            }
            // Reserve before semantic recall too: empty or unchanged corpora must
            // not turn a minute-long reconciliation timer into embedding traffic.
            let mut reservation = ConnectionRecord {
                candidate_id: candidate.candidate_id.clone(),
                fingerprint: String::new(),
                attempted_at: now,
                state: ConnectionState::Reviewing,
                sources: vec![],
                connection: None,
                request_id: String::new(),
                feed_id: String::new(),
                source_route: None,
                decision_policy: None,
                decision_origin: None,
            };
            if !self
                .store
                .reserve_connection_review(&scope.principal, &scope.workspace, &reservation)
                .await?
            {
                continue;
            }
            calls += 1;
            let (sources, route) = match self.current_sources(scope, &candidate).await {
                Ok(Some(input)) => input,
                _ => {
                    reservation.state = ConnectionState::Failed;
                    self.store
                        .put_connection(&scope.principal, &scope.workspace, &reservation)
                        .await?;
                    continue;
                },
            };
            let scoped = router.with_scope_context(Some(magicllm::LlmScope::new(
                &scope.principal,
                &scope.workspace,
            )));
            let decision_policy = ConnectionDecisionPolicy::capture(&scoped).await;
            let revision = decision_policy.fingerprint(&sources);
            if sources.len() < 2
                || old.as_ref().is_some_and(|r| {
                    matches!(r.state, ConnectionState::Empty | ConnectionState::Withdrawn)
                        && r.fingerprint == revision
                })
            {
                reservation.state = ConnectionState::Empty;
                reservation.fingerprint = revision;
                self.store
                    .put_connection(&scope.principal, &scope.workspace, &reservation)
                    .await?;
                continue;
            }
            let identity = blake3::hash(
                serde_json::to_string(&(
                    &scope.principal,
                    &scope.workspace,
                    &candidate.candidate_id,
                    &revision,
                ))
                .unwrap()
                .as_bytes(),
            )
            .to_hex()
            .to_string();
            let mut record = ConnectionRecord {
                candidate_id: candidate.candidate_id.clone(),
                fingerprint: revision,
                attempted_at: now,
                state: ConnectionState::Reviewing,
                sources,
                connection: None,
                request_id: format!("memory-connection-{identity}"),
                feed_id: format!("memory_connection:{identity}"),
                source_route: route,
                decision_policy: None,
                decision_origin: None,
            };
            // Debit before any network I/O; process crashes still spend the call.
            self.store
                .put_connection(&scope.principal, &scope.workspace, &record)
                .await?;
            let source_check_factory = || {
                let check_runtime = self.clone();
                let check_scope = scope.clone();
                let check_candidate_id = record.candidate_id.clone();
                let expected_sources = source_content_digest(&record.sources);
                let source_check: ConnectionSourceCheck = Arc::new(move || {
                    let runtime = check_runtime.clone();
                    let scope = check_scope.clone();
                    let candidate_id = check_candidate_id.clone();
                    let expected = expected_sources.clone();
                    Box::pin(async move {
                        let Ok(Some(candidate)) = runtime
                            .store
                            .get_candidate(&scope.principal, &scope.workspace, &candidate_id)
                            .await
                        else {
                            return false;
                        };
                        runtime
                            .current_sources(&scope, &candidate)
                            .await
                            .ok()
                            .flatten()
                            .is_some_and(|(sources, _)| source_content_digest(&sources) == expected)
                    })
                });
                source_check
            };
            let started = std::time::Instant::now();
            let reply = tokio::time::timeout(
                Duration::from_secs(20),
                review_connection_with_source_check(
                    &scoped,
                    &record.sources,
                    &decision_policy,
                    Some(&source_check_factory),
                ),
            )
            .await;
            if let Some(observer) = observer.as_deref_mut() {
                let (response, error) = match &reply {
                    Ok(Ok(response)) => (Some(response.response.clone()), None),
                    Ok(Err(error)) => (None, Some(format!("{error:#}"))),
                    Err(_) => (None, Some("connection model timed out".into())),
                };
                observer(ConnectionReviewObservation {
                    candidate_id: record.candidate_id.clone(),
                    sources: record.sources.clone(),
                    response,
                    error,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            }
            let parsed = match reply {
                Ok(Ok(reply)) if reply.current() => {
                    record.decision_policy =
                        reply.origin.as_ref().map(|_| decision_policy.key.clone());
                    record.decision_origin = reply.origin;
                    Ok(reply.connection)
                },
                Ok(Ok(_)) => Err(anyhow::anyhow!("connection decision policy changed")),
                _ => Err(anyhow::anyhow!("connection model unavailable")),
            };
            match parsed {
                Ok(Some(connection)) => {
                    record
                        .sources
                        .retain(|s| connection.evidence.iter().any(|c| c.id == s.id));
                    record.connection = Some(connection);
                    record.state = ConnectionState::Ready;
                },
                Ok(None) => {
                    record.state = ConnectionState::Empty;
                    record.sources.clear();
                },
                Err(_) => {
                    record.state = ConnectionState::Failed;
                    record.sources.clear();
                },
            }
            self.store
                .put_connection(&scope.principal, &scope.workspace, &record)
                .await?;
            if record.state == ConnectionState::Ready {
                // Reopen sources after the model call before publishing anything.
                self.reconcile(scope, &mut record, now).await?;
            }
        }
        Ok(calls)
    }

    async fn current_sources(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<Option<(Vec<ConnectionSource>, Option<String>)>> {
        if !matches!(
            candidate.state,
            CandidateState::Candidate | CandidateState::Surfaced
        ) {
            return Ok(None);
        }
        let detail = tokio::time::timeout(
            Duration::from_secs(5),
            self.interactions.resolve_detail(scope, candidate),
        )
        .await??;
        if detail.status != ResurfacingSourceStatus::Available
            || detail.source_updated
            || detail.has_newer
        {
            return Ok(None);
        }
        let memory = self
            .resolver
            .resolve_for_scope(&scope.principal, &scope.workspace)?;
        let activity = activity_source(candidate);
        let mut sources = vec![activity.clone()];
        sources.extend(recall(&memory, &activity.text).await?);
        if let Some(profile) = self.profile_source(scope).await {
            sources.push(profile);
        }
        Ok(Some((sources, detail.source_route)))
    }

    /// Reuse the owner-managed profile; pending capture proposals are never read.
    /// Skip oversized notes as a whole so clipping cannot omit an exception.
    async fn profile_source(&self, scope: &AttentionScope) -> Option<ConnectionSource> {
        let loader = self.taste.as_ref()?;
        let profile = tokio::time::timeout(
            Duration::from_secs(5),
            loader.load(&scope.principal, &scope.workspace),
        )
        .await
        .ok()??;
        if profile.injectable.chars().count() > 4000 {
            return None;
        }
        Some(ConnectionSource {
            id: "owner_profile".into(),
            key: "owner_taste_profile".into(),
            revision: profile.version,
            text: profile.injectable,
        })
    }

    async fn reconcile(
        &self,
        scope: &AttentionScope,
        record: &mut ConnectionRecord,
        now: i64,
    ) -> Result<()> {
        let Some(current_record) = self
            .store
            .get_connection(&scope.principal, &scope.workspace, &record.candidate_id)
            .await?
        else {
            return Ok(());
        };
        if current_record.attempted_at != record.attempted_at
            || current_record.fingerprint != record.fingerprint
        {
            return Ok(());
        }
        if matches!(
            current_record.state,
            ConnectionState::Done | ConnectionState::Withdrawn
        ) {
            *record = current_record;
            return Ok(());
        }
        if current_record.state == ConnectionState::Withdrawing {
            record.state = ConnectionState::Withdrawing;
        }
        if record.state == ConnectionState::Withdrawing {
            return self.withdraw(scope, record).await;
        }
        if let Some(expected) = &record.decision_policy {
            let current = match self.router.as_ref() {
                Some(router) => Some(
                    ConnectionDecisionPolicy::capture_for_reconciliation(
                        &router.with_scope_context(Some(magicllm::LlmScope::new(
                            &scope.principal,
                            &scope.workspace,
                        ))),
                    )
                    .await,
                ),
                None => None,
            };
            if current
                .as_ref()
                .is_none_or(|p| !p.unavailable() && &p.key != expected)
            {
                self.withdraw(scope, record).await?;
                return Ok(());
            }
        }
        let candidate = self
            .store
            .get_candidate(&scope.principal, &scope.workspace, &record.candidate_id)
            .await?;
        let current: Result<bool> = async {
            Ok(match candidate.as_ref() {
                Some(c)
                    if matches!(
                        c.state,
                        CandidateState::Candidate | CandidateState::Surfaced
                    ) =>
                {
                    let detail = tokio::time::timeout(
                        Duration::from_secs(5),
                        self.interactions.resolve_detail(scope, c),
                    )
                    .await??;
                    let activity = activity_source(c);
                    let activity_current = record
                        .sources
                        .iter()
                        .find(|s| s.id == "activity")
                        .is_some_and(|s| s.key == activity.key && s.revision == activity.revision);
                    let memory = self
                        .resolver
                        .resolve_for_scope(&scope.principal, &scope.workspace)?;
                    let keys = record
                        .sources
                        .iter()
                        .filter(|s| s.id != "activity" && s.id != "owner_profile")
                        .map(|s| (s.key.clone(), s.revision.clone()))
                        .collect::<Vec<_>>();
                    let profile_current =
                        match record.sources.iter().find(|s| s.id == "owner_profile") {
                            Some(saved) => self.profile_source(scope).await.as_ref() == Some(saved),
                            None => true,
                        };
                    activity_current
                        && profile_current
                        && detail.status == ResurfacingSourceStatus::Available
                        && !detail.source_updated
                        && !detail.has_newer
                        && (keys.is_empty()
                            || user_memory_sources_are_current(&memory, &keys).await?)
                },
                _ => false,
            })
        }
        .await;
        if now - record.attempted_at > 7 * 86400 || !current.unwrap_or(false) {
            self.withdraw(scope, record).await?;
            return Ok(());
        }
        let candidate = candidate.unwrap();
        if self
            .feed
            .attention_item_is_dismissed(
                &scope.principal,
                &scope.workspace,
                &format!("today:changed:{}", record.feed_id),
            )
            .await?
        {
            self.record_feedback(scope, record, FeedbackAction::Dismiss, now, "card_dismiss")
                .await?;
            self.withdraw(scope, record).await?;
            record.state = ConnectionState::Done;
            self.store
                .put_connection(&scope.principal, &scope.workspace, record)
                .await?;
            return Ok(());
        }
        if let Some(requests) = &self.requests {
            let history = requests.list_history_for_scope(&scope.principal, &scope.workspace, None);
            if let Some(answer) = history
                .iter()
                .find(|r| r.request.id == record.request_id)
                .and_then(|r| r.response.as_ref())
            {
                self.apply_answer(scope, record, answer, now).await?;
                return Ok(());
            }
        }
        if record.state == ConnectionState::Published {
            return Ok(());
        }
        let connection = record
            .connection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing connection"))?;
        let is_hitl = connection.surface == ConnectionSurface::Hitl;
        if is_hitl && self.requests.is_none() {
            return Ok(());
        }
        if let Some(expected) = &record.decision_policy {
            let current = match self.router.as_ref() {
                Some(router) => Some(
                    ConnectionDecisionPolicy::capture_for_reconciliation(
                        &router.with_scope_context(Some(magicllm::LlmScope::new(
                            &scope.principal,
                            &scope.workspace,
                        ))),
                    )
                    .await,
                ),
                None => None,
            };
            if current.as_ref().is_some_and(|p| p.unavailable()) {
                return Ok(());
            }
            if current.as_ref().is_none_or(|p| &p.key != expected) {
                self.withdraw(scope, record).await?;
                return Ok(());
            }
        }
        let normalized = AttentionCandidate {
            candidate_key: record.request_id.clone(),
            source: AttentionSource {
                kind: AttentionSourceKind::Memory,
                source_ref: record.candidate_id.clone(),
                provider: None,
                account_alias: None,
            },
            source_family: AttentionSourceFamily::Memory,
            evidence_refs: record.sources.iter().map(|s| s.key.clone()).collect(),
            title: connection.title.clone(),
            summary: connection.summary.clone(),
            action: None,
            urgency: AttentionUrgency::Low,
            confidence: Some(connection.confidence),
            metadata: json!({"producer":OPERATION,"inference":true,"surface":connection.surface}),
        };
        let outcome = route_attention_candidate(
            &normalized,
            &AttentionRouteContext {
                owner_intervention_required: is_hitl,
                // For you already presents AgentLearning in the Changed lane.
                observed_change: connection.surface == ConnectionSurface::ForYou,
                non_actionable_useful_context: connection.surface == ConnectionSurface::WorthALook,
                ..Default::default()
            },
        );
        ensure!(
            matches!(outcome, RouteOutcome::Routed { .. }),
            "connection not accepted by attention router"
        );
        if connection.surface != ConnectionSurface::WorthALook
            && !self
                .store
                .hold_connection_candidate(
                    &scope.principal,
                    &scope.workspace,
                    &candidate,
                    record.attempted_at + 7 * 86400,
                )
                .await?
        {
            self.withdraw(scope, record).await?;
            return Ok(());
        }
        match connection.surface {
            ConnectionSurface::Hitl => {
                let request = UserRequest {
                    id: record.request_id.clone(),
                    request_type: REQUEST_TYPE.into(),
                    question: format!(
                        "{}\n\n{}",
                        connection.question.as_deref().unwrap_or_default(),
                        evidence_text(record)
                    ),
                    options: vec![
                        RequestOption {
                            id: "acknowledge".into(),
                            label: "Got it".into(),
                            requires_input: false,
                        },
                        RequestOption {
                            id: "remember".into(),
                            label: "Remember my clarification".into(),
                            requires_input: true,
                        },
                        RequestOption {
                            id: "dismiss".into(),
                            label: "Dismiss".into(),
                            requires_input: false,
                        },
                    ],
                    principal: scope.principal.clone(),
                    workspace: scope.workspace.clone(),
                    context: json!({"producer":OPERATION,"connection_id":record.request_id,"candidate_id":record.candidate_id,"sources":record.sources,"inference":true}),
                    source: OPERATION.into(),
                    execution_id: None,
                    task_id: None,
                    timeout_secs: 7 * 86400,
                    default_on_timeout: "dismiss".into(),
                    created_at: record.attempted_at * 1000,
                    sensitive: None,
                };
                self.requests
                    .as_ref()
                    .unwrap()
                    .submit_nonblocking_durable(request)
                    .await?;
            },
            ConnectionSurface::WorthALook => {
                // The existing item's Open/Dismiss/contextual-action targets remain real.
                self.store
                    .upsert_phrasing(
                        &scope.principal,
                        &scope.workspace,
                        &record.candidate_id,
                        &connection.title,
                        &evidence_text(record),
                        candidate.content_revision.as_deref(),
                        now,
                    )
                    .await?;
                self.store
                    .mark_surfaced(
                        &scope.principal,
                        &scope.workspace,
                        &[record.candidate_id.clone()],
                        now,
                    )
                    .await?;
            },
            ConnectionSurface::ForYou => {
                let item = self.feed_item(scope, record, None, now);
                self.feed.upsert_item(item).await?;
            },
        }
        if let Some(store) = &self.attention {
            store.append_event(AttentionRouteEvent {
                event_id: record.request_id.clone(), scope:scope.clone(), source:normalized.source,
                source_family:normalized.source_family, candidate_key:normalized.candidate_key,
                stage:AttentionFunnelStage::Surfaced, outcome,occurred_at:now*1000,created_at:now*1000,
                confidence:normalized.confidence,metadata:json!({"producer":OPERATION,"surface":connection.surface,"evidence_refs":normalized.evidence_refs}),
            }).await?;
        }
        record.state = ConnectionState::Published;
        self.store
            .put_connection(&scope.principal, &scope.workspace, record)
            .await
    }

    fn feed_item(
        &self,
        scope: &AttentionScope,
        record: &ConnectionRecord,
        answer: Option<&str>,
        now: i64,
    ) -> FeedItem {
        let connection = record.connection.as_ref().expect("validated connection");
        FeedItem {
            id: record.feed_id.clone(),
            principal: scope.principal.clone(),
            workspace: scope.workspace.clone(),
            item_type: FeedItemType::AgentLearning,
            task_id: None,
            ui_thread_id: None,
            agent_id: None,
            title: connection.title.clone(),
            summary: Some(match answer {
                Some(text) => format!("{text}\n\n{}", evidence_text(record)),
                None => evidence_text(record),
            }),
            status: FeedItemStatus::Info,
            created_at: record.attempted_at * 1000,
            updated_at: now * 1000,
            actions: Vec::new(),
            metadata: json!({"card_kind":"agent_learning","tier":"memory_connections","label":"Related memories","inference":true,"connection_id":record.request_id,"candidate_id":record.candidate_id,"sources":record.sources,"evidence_refs":record.sources.iter().map(|s|json!({"source_ref":s.key,"revision":s.revision})).collect::<Vec<_>>(),"source_route":record.source_route,"expires_at":(record.attempted_at+7*86400)*1000}),
        }
    }

    async fn apply_answer(
        &self,
        scope: &AttentionScope,
        record: &mut ConnectionRecord,
        answer: &UserResponse,
        now: i64,
    ) -> Result<()> {
        let valid_input = answer
            .input
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.chars().count() <= 2000);
        let invalid_clarification =
            answer.decision == "remember" && (valid_input.is_none() || answer.channel == "timeout");
        let decision = if invalid_clarification {
            "dismiss"
        } else {
            answer.decision.as_str()
        };
        if invalid_clarification {
            self.feed
                .upsert_item(self.feed_item(
                    scope,
                    record,
                    Some("No clarification was saved: the answer was empty, too long, or expired."),
                    now,
                ))
                .await?;
        }
        if decision == "remember" && answer.channel != "timeout" {
            let text = answer
                .input
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.chars().count() <= 2000)
                .ok_or_else(|| anyhow::anyhow!("missing or excessive owner clarification"))?;
            // Save the owner's words with the context they answered. The shared
            // lifecycle reviews corrections/exceptions instead of leaving an
            // unrelated clarification permanently beside conflicting memories.
            let key = format!("connection_clarification:{}", record.request_id);
            let fields = serde_json::Map::from_iter([(
                key.clone(),
                json!({"key":key,"value":text,"source_type":"explicit_user_statement","confidence":1.0,"updated_at":chrono::DateTime::from_timestamp_millis(record.attempted_at*1000).unwrap().to_rfc3339(),"source_event_id":format!("connection_answer:{}",record.request_id),"source_ids":record.sources.iter().map(|s|s.key.clone()).collect::<Vec<_>>(),"clarification_context":record.sources.iter().map(|s|s.text.clone()).collect::<Vec<_>>(),"rationale":"Owner clarification in response to a memory connection; applies to the cited context."}),
            )]);
            let saved = merge_user_memory_tier_fields(
                &self.resolver,
                &scope.principal,
                &scope.workspace,
                "user_preferences",
                &fields,
            )
            .await;
            ensure!(
                saved.get("status").and_then(serde_json::Value::as_str) == Some("ok"),
                "owner clarification was not persisted"
            );
            self.feed
                .upsert_item(self.feed_item(
                    scope,
                    record,
                    Some(&format!("Remembered your clarification: {text}")),
                    now,
                ))
                .await?;
        }
        let action = if matches!(decision, "acknowledge" | "remember") {
            FeedbackAction::Acknowledge
        } else {
            FeedbackAction::Dismiss
        };
        self.record_feedback(scope, record, action, now, "answer")
            .await?;
        record.state = ConnectionState::Done;
        self.store
            .put_connection(&scope.principal, &scope.workspace, record)
            .await
    }

    async fn record_feedback(
        &self,
        scope: &AttentionScope,
        record: &ConnectionRecord,
        action: FeedbackAction,
        now: i64,
        event: &str,
    ) -> Result<()> {
        use magician::magician_v2::attention::resurfacing::types::{
            DEFAULT_ACK_COOLDOWN_SECS, DEFAULT_DISMISS_COOLDOWN_SECS,
        };
        self.store
            .record_action_with_reason_event(
                &scope.principal,
                &scope.workspace,
                &record.candidate_id,
                action,
                None,
                Some(&format!("{}:{event}", record.request_id)),
                now,
                DEFAULT_DISMISS_COOLDOWN_SECS,
                DEFAULT_ACK_COOLDOWN_SECS,
            )
            .await
    }

    async fn withdraw(&self, scope: &AttentionScope, record: &mut ConnectionRecord) -> Result<()> {
        if record.state != ConnectionState::Withdrawing {
            record.state = ConnectionState::Withdrawing;
            self.store
                .put_connection(&scope.principal, &scope.workspace, record)
                .await?;
        }
        self.store
            .clear_connection_phrasing(&scope.principal, &scope.workspace, record)
            .await?;
        self.store
            .release_connection_hold(&scope.principal, &scope.workspace, record)
            .await?;
        self.feed
            .remove_item(&scope.principal, &scope.workspace, &record.feed_id)
            .await?;
        if let Some(requests) = &self.requests {
            if requests
                .pending_request_snapshot(
                    &record.request_id,
                    Some(&scope.principal),
                    Some(&scope.workspace),
                )
                .await
                .is_some()
            {
                let result = requests
                    .respond_scoped(
                        UserResponse {
                            request_id: record.request_id.clone(),
                            decision: "dismiss".into(),
                            input: None,
                            channel: "memory_connection_withdrawal".into(),
                            sensitive: Vec::new(),
                        },
                        Some(&scope.principal),
                        Some(&scope.workspace),
                    )
                    .await;
                ensure!(
                    result != ScopedResponseResult::PersistenceUnavailable,
                    "could not withdraw stale question"
                );
            }
        }
        record.state = ConnectionState::Withdrawn;
        self.store
            .put_connection(&scope.principal, &scope.workspace, record)
            .await
    }
}
