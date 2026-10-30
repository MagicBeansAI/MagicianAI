//! Conservative repair from retained, scope-bound chat sources. Assessment is
//! the default; missing or ambiguous originals remain explicitly unrecoverable.
use super::{
    models::{ChatMessage, ChatMessageContent, ChatMessageDirection},
    storage::ChatStore,
};
use crate::magician_v2::{agents::AgentMemoryService, artifact_v2::memory::V3EpisodeRecord};
use serde_json::{json, Value};

fn missing_source_field(value: Option<&Value>) -> bool {
    value.is_none_or(|value| {
        value.is_null() || value.as_str().is_some_and(|text| text.trim().is_empty())
    })
}

fn repair_from_messages(
    episode: &V3EpisodeRecord,
    messages: &[ChatMessage],
) -> (V3EpisodeRecord, usize, usize) {
    let mut repaired = episode.clone();
    let mut recovered = 0;
    let mut unavailable = 0;
    let start = chrono::DateTime::parse_from_rfc3339(&episode.started_at)
        .ok()
        .map(|value| value.timestamp_millis());
    let end = chrono::DateTime::parse_from_rfc3339(&episode.completed_at)
        .ok()
        .map(|value| value.timestamp_millis());
    for candidate in &mut repaired.memory_candidates {
        let (role, direction) = match candidate.candidate_type.as_str() {
            "chat_user_message_excerpt" => ("user", ChatMessageDirection::User),
            "chat_assistant_reply_excerpt" => ("assistant", ChatMessageDirection::Assistant),
            _ => continue,
        };
        if magician_vector_index::episode_candidates::conversation_source_text(&candidate.value)
            .is_some_and(|(_, complete)| complete)
        {
            continue;
        }
        if !missing_source_field(candidate.value.get("source_text"))
            || (!missing_source_field(candidate.value.get("source_role"))
                && candidate.value.get("source_role").and_then(Value::as_str) != Some(role))
        {
            unavailable += 1;
            continue;
        }
        let Some(excerpt) = candidate.value.get("text_excerpt").and_then(Value::as_str) else {
            unavailable += 1;
            continue;
        };
        // Only the known old renderer's exact truncation shape is a prefix.
        // A naturally ellipsised short message must match in full.
        let truncated = excerpt.chars().count() == 4003 && excerpt.ends_with("...");
        let prefix = if truncated {
            excerpt.strip_suffix("...").unwrap()
        } else {
            excerpt
        };
        let matches = messages
            .iter()
            .filter_map(|message| {
                if message.direction != direction || message.context_origin.is_some() {
                    return None;
                }
                let (Some(start), Some(end)) = (start, end) else {
                    return None;
                };
                if message.created_at < start.saturating_sub(60_000)
                    || message.created_at > end.saturating_add(30_000)
                {
                    return None;
                }
                let ChatMessageContent::Text { text, .. } = &message.content else {
                    return None;
                };
                // The episode kept only the owner's words; read the stored
                // message the same way so the two still match.
                let sanitized = super::service::redact_chat_memory_text(
                    &super::authored_framing::owner_words(text),
                );
                // The compatibility renderer trimmed the source before
                // clipping. Match that view but retain the complete original.
                (if truncated {
                    sanitized.trim().starts_with(prefix)
                } else {
                    sanitized.trim() == prefix
                })
                .then_some((message, sanitized))
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 || prefix.trim().is_empty() {
            unavailable += 1;
            continue;
        }
        let (message, text) = &matches[0];
        let source_id = candidate
            .value
            .get("source_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| candidate.key.clone())
            .unwrap_or_else(|| format!("{}:{role}", episode.episode_id));
        candidate.value["source_text"] = json!(text);
        candidate.value["source_role"] = json!(role);
        for (field, value) in [
            ("source_id", json!(source_id)),
            (
                "source_recorded_at",
                json!(
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(message.created_at)
                        .map(|at| at.to_rfc3339())
                ),
            ),
            (
                "source_repair",
                json!({"version":1,"message_id":message.id,"session_id":message.session_id}),
            ),
        ] {
            if missing_source_field(candidate.value.get(field)) {
                candidate.value[field] = value;
            }
        }
        recovered += 1;
    }
    (repaired, recovered, unavailable)
}

pub async fn repair_legacy_chat_sources(
    service: &AgentMemoryService,
    store: &dyn ChatStore,
    agent_id: &str,
    apply: bool,
    limit: usize,
) -> anyhow::Result<Value> {
    repair_legacy_chat_sources_page(service, store, agent_id, apply, limit, None).await
}

pub async fn repair_legacy_chat_sources_page(
    service: &AgentMemoryService,
    store: &dyn ChatStore,
    agent_id: &str,
    apply: bool,
    limit: usize,
    after_episode_id: Option<&str>,
) -> anyhow::Result<Value> {
    let (principal, workspace) = service
        .scoped_memory_scope()
        .ok_or_else(|| anyhow::anyhow!("source repair requires an explicit scope"))?;
    let mut episodes = service.load_native_episodes(agent_id).await?;
    episodes.sort_by(|left, right| left.episode_id.cmp(&right.episode_id));
    let mut assessed = 0;
    let mut recovered = 0;
    let mut written = 0;
    let mut unavailable = 0;
    let mut stale = 0;
    let mut details = Vec::new();
    let eligible = episodes
        .iter()
        .filter(|episode| episode.goal_key.starts_with("chat:"))
        .filter(|episode| after_episode_id.is_none_or(|after| episode.episode_id.as_str() > after))
        .collect::<Vec<_>>();
    let batch = &eligible[..eligible.len().min(limit.clamp(1, 1000))];
    for episode in batch {
        assessed += 1;
        let legacy = episode
            .memory_candidates
            .iter()
            .filter(|candidate| {
                matches!(
                    candidate.candidate_type.as_str(),
                    "chat_user_message_excerpt" | "chat_assistant_reply_excerpt"
                ) && !magician_vector_index::episode_candidates::conversation_source_text(
                    &candidate.value,
                )
                .is_some_and(|(_, complete)| complete)
            })
            .count();
        if legacy == 0 {
            continue;
        }
        let session_id = episode.goal_key.strip_prefix("chat:").unwrap();
        let Some(snapshot) = store.memory_repair_snapshot(session_id).await? else {
            unavailable += legacy;
            details.push(json!({"episode_id":episode.episode_id,"status":"original_unavailable","sources":legacy}));
            continue;
        };
        if snapshot.session.principal != principal
            || snapshot.session.workspace != workspace
            || snapshot.session.agent_id != agent_id
            || episode
                .principal
                .as_deref()
                .is_some_and(|value| value != principal)
            || episode
                .workspace
                .as_deref()
                .is_some_and(|value| value != workspace)
        {
            unavailable += legacy;
            details.push(json!({"episode_id":episode.episode_id,"status":"scope_mismatch"}));
            continue;
        }
        let (repaired, count, missing) = repair_from_messages(episode, &snapshot.messages);
        recovered += count;
        unavailable += missing;
        if count > 0 && apply {
            if service
                .repair_native_episode_sources(episode, &repaired)
                .await?
            {
                written += 1;
            } else {
                stale += 1;
            }
        }
        details.push(json!({"episode_id":episode.episode_id,"recoverable_sources":count,"unrecoverable_sources":missing}));
        // snapshot holds the retained transcript's lifecycle fence until here.
    }
    Ok(
        json!({"status":"ok","mode":if apply { "repair" } else { "assessment" },"assessed_episodes":assessed,
        "recoverable_sources":recovered,"repaired_episodes":written,"unrecoverable_sources":unavailable,"stale_episodes":stale,
        "remaining_episodes":eligible.len().saturating_sub(assessed),
        "next_after_episode_id":if eligible.len() > assessed { batch.last().map(|episode| episode.episode_id.as_str()) } else { None },"details":details}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn memory_recall_repair_assessment_and_atomic_retry_use_retained_chat_owner() {
        use super::super::{models::ChatChannel, storage::FileChatStore};
        use crate::magician_v2::{
            agents::{memory::EpisodeOutcome, AgentMemoryResolver},
            artifact_v2::memory::MemoryCandidate,
        };
        let temp = tempfile::tempdir().unwrap();
        let store = FileChatStore::with_index(temp.path()).await.unwrap();
        let session = store
            .new_session(
                "owner",
                "home",
                "general",
                &ChatChannel::new("web", "owner"),
                "personal-assistant",
            )
            .await
            .unwrap();
        let resolver = AgentMemoryResolver::new(temp.path());
        let service = resolver.resolve_for_scope("owner", "home").unwrap();
        let now = chrono::Utc::now();
        let text = format!("{} I attended The Glass Menagerie.", "話".repeat(4500));
        store
            .append_message(
                &session.id,
                ChatMessage::new(
                    "original",
                    &session.id,
                    ChatMessageDirection::User,
                    ChatMessageContent::Text {
                        text,
                        plan_reply: None,
                    },
                    now.timestamp_millis(),
                ),
            )
            .await
            .unwrap();
        let mut episode = V3EpisodeRecord::new_memory_episode(
            Some(("owner", "home")),
            "personal-assistant",
            "legacy",
            format!("chat:{}", session.id),
            "chat_inline_turn",
            1,
            now,
            None,
            now,
            now,
            &EpisodeOutcome::GoalAchieved {
                summary: "Saved".into(),
            },
            vec![],
            vec![],
            vec![],
            None,
            None,
            None,
        );
        episode.memory_candidates.push(MemoryCandidate {
            candidate_type: "chat_user_message_excerpt".into(),
            target_hint: String::new(),
            key: Some("legacy:user".into()),
            value: json!({"text_excerpt":format!("{}...", "話".repeat(4000))}),
            confidence: 0.8,
            source: "chat_user_turn".into(),
            evidence: vec![],
            rationale: String::new(),
        });
        service
            .append_native_episode("personal-assistant", &episode)
            .await
            .unwrap();
        let assessment =
            repair_legacy_chat_sources(&service, &store, "personal-assistant", false, 100)
                .await
                .unwrap();
        assert_eq!(assessment["recoverable_sources"], 1);
        assert_eq!(assessment["repaired_episodes"], 0);
        assert!(service
            .load_native_episodes("personal-assistant")
            .await
            .unwrap()[0]
            .memory_candidates[0]
            .value
            .get("source_text")
            .is_none());
        let report = repair_legacy_chat_sources(&service, &store, "personal-assistant", true, 100)
            .await
            .unwrap();
        assert_eq!(report["repaired_episodes"], 1);
        let restored = service
            .load_native_episodes("personal-assistant")
            .await
            .unwrap()
            .remove(0);
        assert!(restored.memory_candidates[0].value["source_text"]
            .as_str()
            .unwrap()
            .contains("The Glass Menagerie"));
        assert!(
            !service
                .repair_native_episode_sources(&episode, &restored)
                .await
                .unwrap(),
            "stale assessment cannot replace the repaired revision"
        );
        let repeated =
            repair_legacy_chat_sources(&service, &store, "personal-assistant", true, 100)
                .await
                .unwrap();
        assert_eq!(repeated["repaired_episodes"], 0);
        assert_eq!(repeated["unrecoverable_sources"], 0);
        let mut later = episode.clone();
        later.episode_id = "zzz-later".into();
        service
            .append_native_episode("personal-assistant", &later)
            .await
            .unwrap();
        let first =
            repair_legacy_chat_sources_page(&service, &store, "personal-assistant", false, 1, None)
                .await
                .unwrap();
        assert_eq!(first["remaining_episodes"], 1);
        assert_eq!(first["next_after_episode_id"], "legacy");
        let second = repair_legacy_chat_sources_page(
            &service,
            &store,
            "personal-assistant",
            false,
            1,
            first["next_after_episode_id"].as_str(),
        )
        .await
        .unwrap();
        assert_eq!(second["recoverable_sources"], 1);
        assert!(second["next_after_episode_id"].is_null());
        let mut invalid = restored.clone();
        invalid.memory_candidates[0].value["text_excerpt"] = json!("changed old evidence");
        assert!(service
            .repair_native_episode_sources(&restored, &invalid)
            .await
            .is_err());
        for (index, placeholder) in [Value::Null, json!("  ")].into_iter().enumerate() {
            let mut partial = episode.clone();
            partial.episode_id = format!("nullable-source-{index}");
            partial.memory_candidates[0].value["source_text"] = placeholder;
            partial.memory_candidates[0].value["source_role"] = Value::Null;
            partial.memory_candidates[0].value["source_recorded_at"] = Value::Null;
            partial.memory_candidates[0].value["source_id"] = json!("retained-source:user");
            service
                .append_native_episode("personal-assistant", &partial)
                .await
                .unwrap();
        }
        let report = repair_legacy_chat_sources_page(
            &service,
            &store,
            "personal-assistant",
            true,
            2,
            Some("legacy"),
        )
        .await
        .unwrap();
        assert_eq!(report["repaired_episodes"], 2);
        for partial in service
            .load_native_episodes("personal-assistant")
            .await
            .unwrap()
            .into_iter()
            .filter(|episode| episode.episode_id.starts_with("nullable-source-"))
        {
            assert_eq!(
                partial.memory_candidates[0].value["source_id"],
                "retained-source:user"
            );
            assert_eq!(partial.memory_candidates[0].value["source_role"], "user");
            assert!(partial.memory_candidates[0].value["source_text"]
                .as_str()
                .unwrap()
                .contains("The Glass Menagerie"));
            assert!(partial.memory_candidates[0].value["source_recorded_at"]
                .as_str()
                .is_some());
        }
        let repeated = repair_legacy_chat_sources_page(
            &service,
            &store,
            "personal-assistant",
            true,
            2,
            Some("legacy"),
        )
        .await
        .unwrap();
        assert_eq!(repeated["repaired_episodes"], 0);
        assert_eq!(repeated["unrecoverable_sources"], 0);
    }
    #[test]
    fn memory_recall_repair_is_redacted_idempotent_and_refuses_ambiguity() {
        use crate::magician_v2::agents::memory::EpisodeOutcome;
        use crate::magician_v2::artifact_v2::memory::MemoryCandidate;
        let now = chrono::Utc::now();
        let full = format!(
            " \n{}I now live in Mumbai. api_key=top-secret\n ",
            "文".repeat(4500)
        );
        let excerpt = format!("{}...", "文".repeat(4000));
        let mut episode = V3EpisodeRecord::new_memory_episode(
            Some(("owner", "home")),
            "personal-assistant",
            "episode",
            "chat:session",
            "chat_inline_turn",
            1,
            now,
            None,
            now,
            now,
            &EpisodeOutcome::GoalAchieved {
                summary: "Done".into(),
            },
            vec![],
            vec![],
            vec![],
            None,
            None,
            None,
        );
        episode.memory_candidates.push(MemoryCandidate {
            candidate_type: "chat_user_message_excerpt".into(),
            value: json!({"text_excerpt":excerpt}),
            target_hint: String::new(),
            key: None,
            confidence: 0.8,
            source: "chat_user_turn".into(),
            evidence: vec![],
            rationale: String::new(),
        });
        let message: ChatMessage = serde_json::from_value(json!({"id":"message","session_id":"session","direction":"user","content":{"type":"text","text":full},"created_at":now.timestamp_millis()})).unwrap();
        let (repaired, count, missing) = repair_from_messages(&episode, &[message.clone()]);
        assert_eq!((count, missing), (1, 0));
        let text = repaired.memory_candidates[0].value["source_text"]
            .as_str()
            .unwrap();
        assert!(text.contains("Mumbai"));
        assert!(!text.contains("top-secret"));
        assert!(text.starts_with(" \n"));
        assert!(text.ends_with("\n "));
        assert_eq!(repair_from_messages(&repaired, &[message.clone()]).1, 0);
        for (field, value) in [
            ("source_text", json!(false)),
            ("source_role", json!("assistant")),
        ] {
            let mut unsupported = episode.clone();
            unsupported.memory_candidates[0].value[field] = value;
            let (unchanged, count, missing) =
                repair_from_messages(&unsupported, &[message.clone()]);
            assert_eq!((count, missing), (0, 1));
            assert_eq!(
                unchanged.memory_candidates[0].value,
                unsupported.memory_candidates[0].value
            );
        }
        let mut metadata = episode.clone();
        metadata.memory_candidates[0].value["source_id"] = json!("original-id");
        metadata.memory_candidates[0].value["source_recorded_at"] = json!("original-capture-time");
        metadata.memory_candidates[0].value["source_repair"] = json!({"original_metadata":true});
        let (restored, count, missing) = repair_from_messages(&metadata, &[message.clone()]);
        assert_eq!((count, missing), (1, 0));
        for field in ["source_id", "source_recorded_at", "source_repair"] {
            assert_eq!(
                restored.memory_candidates[0].value[field],
                metadata.memory_candidates[0].value[field]
            );
        }
        assert_eq!(
            repair_from_messages(&episode, &[message.clone(), message]).1,
            0
        );
        assert_eq!(repair_from_messages(&episode, &[]).2, 1);
    }
}
