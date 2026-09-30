// Test file for prompt storage functionality

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use tempfile::tempdir;
    use tokio::fs;

    use crate::prompts::{
        constants::{names as prompt_names, versions as prompt_versions},
        json_storage::JsonStorageConfig,
        JsonPromptStorage, PromptManager, PromptStore,
    };

    async fn create_real_storage() -> JsonPromptStorage {
        let config = JsonStorageConfig {
            storage_dir: crate::prompts::json_storage::default_prompt_dir(),
            enable_cache: false,
            max_cache_entries: 100,
        };
        let storage = JsonPromptStorage::new(config).unwrap();
        storage.initialize().await.unwrap();
        storage
    }

    async fn create_temp_storage_with_unified_analysis_prompt() -> JsonPromptStorage {
        #[allow(deprecated)]
        let temp_dir = tempdir().expect("failed to create temp dir").into_path();
        fs::create_dir_all(&temp_dir)
            .await
            .expect("failed to create prompt fixture dir");

        let prompt_filename = format!(
            "{}_v{}.json",
            prompt_names::UNIFIED_ANALYSIS,
            prompt_versions::UNIFIED_ANALYSIS
        );

        let source_path = crate::prompts::json_storage::default_prompt_dir().join(&prompt_filename);
        let dest_path = temp_dir.join(&prompt_filename);

        fs::copy(&source_path, &dest_path)
            .await
            .expect("failed to copy unified analysis prompt fixture");

        let config = JsonStorageConfig {
            storage_dir: temp_dir,
            enable_cache: false,
            max_cache_entries: 10,
        };
        let storage = JsonPromptStorage::new(config).unwrap();
        storage.initialize().await.unwrap();
        storage
    }

    #[tokio::test]
    async fn test_json_storage_initialization() {
        let storage = create_real_storage().await;
        assert!(storage.health_check().await.unwrap());
    }

    #[tokio::test]
    async fn test_prompt_manager_with_fallback() {
        let storage = create_temp_storage_with_unified_analysis_prompt().await;
        let manager = PromptManager::new(Arc::new(storage));
        manager.initialize().await.unwrap();

        // Test prompt retrieval from the copied fixture
        let prompt = manager
            .get_prompt(
                prompt_names::UNIFIED_ANALYSIS,
                prompt_versions::UNIFIED_ANALYSIS,
            )
            .await
            .unwrap();
        assert_eq!(prompt.name, prompt_names::UNIFIED_ANALYSIS);
        assert_eq!(prompt.version, prompt_versions::UNIFIED_ANALYSIS);
        assert!(prompt
            .content
            .contains("Perform a comprehensive analysis of this user message"));
    }

    #[tokio::test]
    async fn test_prompt_rendering() {
        let storage = create_real_storage().await;
        let manager = PromptManager::new(Arc::new(storage));
        manager.initialize().await.unwrap();

        let mut variables = HashMap::new();
        variables.insert("query".to_string(), "Deploy my app".to_string());
        variables.insert(
            "categories_context".to_string(),
            "Available categories: deployment, cloud".to_string(),
        );
        variables.insert(
            "tools_description".to_string(),
            "- shell | category=shell_operations | required_params=command | Run shell commands\n- browser | category=browser_automation | required_params=action | Browser automation".to_string(),
        );

        let rendered = manager
            .get_rendered_prompt(
                prompt_names::UNIFIED_ANALYSIS,
                prompt_versions::UNIFIED_ANALYSIS,
                variables,
            )
            .await
            .unwrap();

        assert!(rendered.contains("Deploy my app"));
        assert!(rendered.contains("Available categories: deployment, cloud"));
        assert!(rendered.contains("shell"));
    }

    #[tokio::test]
    async fn test_list_prompt_names() {
        let storage = create_real_storage().await;
        let manager = PromptManager::new(Arc::new(storage));
        manager.initialize().await.unwrap();

        let names = manager.list_prompt_names().await.unwrap();
        assert!(names.contains(&prompt_names::UNIFIED_ANALYSIS.to_string()));
    }

    #[tokio::test]
    async fn voice_modality_addendum_requires_runtime_memory_authority() {
        let storage = create_real_storage().await;
        let manager = PromptManager::new(Arc::new(storage));
        manager.initialize().await.unwrap();

        let mut variables = HashMap::new();
        variables.insert(
            "memory_authority_instruction".to_string(),
            "OWNER_MEMORY_AUTHORITY_SENTINEL".to_string(),
        );
        let rendered = manager
            .get_rendered_prompt(
                prompt_names::VOICE_MODALITY_ADDENDUM,
                prompt_versions::VOICE_MODALITY_ADDENDUM,
                variables,
            )
            .await
            .expect("versioned voice modality addendum should render");

        assert!(rendered.contains("OWNER_MEMORY_AUTHORITY_SENTINEL"));
        assert!(!rendered.contains("{memory_authority_instruction}"));
        assert!(rendered.contains("Everything else from the orchestrator instructions"));
    }

    #[tokio::test]
    async fn voice_live_mouth_system_keeps_openai_policy_labels() {
        let storage = create_real_storage().await;
        let manager = PromptManager::new(Arc::new(storage));
        manager.initialize().await.unwrap();

        let mut variables = HashMap::new();
        variables.insert("assistant_name".to_string(), "Magican".to_string());
        let rendered = manager
            .get_rendered_prompt(
                prompt_names::VOICE_LIVE_MOUTH_SYSTEM,
                prompt_versions::VOICE_LIVE_MOUTH_SYSTEM,
                variables,
            )
            .await
            .expect("versioned GPT-Live mouth prompt should render");

        assert!(rendered.contains("You are Magican,"));
        assert!(rendered.contains("Backchannel policy:"));
        assert!(rendered.contains("Interruption policy:"));
        assert!(rendered.contains("Delegation policy:"));
        assert!(rendered.contains("Delegate to the backend when:"));
        assert!(rendered.contains("Do not delegate to the backend when:"));
        assert!(!rendered.contains("{assistant_name}"));
    }

    #[tokio::test]
    async fn voice_meeting_presto_system_matches_compiled_fallback() {
        let storage = create_real_storage().await;
        let manager = PromptManager::new(Arc::new(storage));
        manager.initialize().await.unwrap();

        let rendered = manager
            .get_rendered_prompt(
                prompt_names::VOICE_MEETING_PRESTO_SYSTEM,
                prompt_versions::VOICE_MEETING_PRESTO_SYSTEM,
                HashMap::new(),
            )
            .await
            .expect("versioned meeting Presto prompt should render");

        assert!(rendered.contains("You are Presto"));
        assert!(rendered.contains("only speak when a participant addresses you"));
        assert!(rendered.contains("1-2 short spoken sentences"));
    }

    #[tokio::test]
    async fn agentic_decision_metadata_prompt_resolves_compact_contracts() {
        let storage = create_real_storage().await;
        let manager = PromptManager::new(Arc::new(storage));

        let prompt = manager
            .get_prompt(
                prompt_names::AGENTIC_DECISION,
                prompt_versions::AGENTIC_DECISION,
            )
            .await
            .expect("current agentic decision prompt");

        assert_eq!(prompt.version, "1.3.7");
        assert!(prompt.content.contains("canonical `none` action"));
        assert!(prompt.content.contains("Do not emit an explicit"));
        assert!(prompt.content.contains("source_iteration_range"));
        assert!(prompt.content.contains("## DECISION METADATA"));
        assert!(prompt.content.contains("request_hover_discovery"));
        assert!(prompt.content.contains("step_completed"));
        assert!(prompt.content.contains("not a second tool call"));
        assert!(prompt.content.contains("call `yield`"));
        assert!(prompt.content.contains("native tool catalog"));
        assert!(prompt.content.contains("Call `http` directly"));
        assert!(prompt.content.contains("retry_after_minutes"));
        assert!(prompt.content.contains("`external_action`"));
        assert!(!prompt.content.contains("\"decision\": \"execute\""));
        assert!(!prompt.content.contains("\"decision\": \"need_user_input\""));
        assert!(!prompt.content.contains("action_type"));
        assert!(!prompt.content.contains("capability_name"));
        assert!(!prompt.content.contains("shapes above"));
        assert!(!prompt.content.contains("goal_reached"));
        assert!(!prompt.content.contains("cannot_proceed"));

        let system_prompt = manager
            .get_prompt(
                prompt_names::AGENTIC_DECISION_SYSTEM,
                prompt_versions::AGENTIC_DECISION_SYSTEM,
            )
            .await
            .expect("current agentic decision system prompt");
        assert_eq!(system_prompt.version, "1.0.7");
        // The owner taste profile rides the system prompt because it is the
        // cache-stable prefix; the slot renders empty when no profile exists.
        assert!(system_prompt.content.contains("{taste_profile_section}"));
        assert!(system_prompt.content.contains("`yield`"));
        assert!(system_prompt
            .content
            .contains("current native tool catalog"));
        assert!(system_prompt.content.contains("call `need_user_input`"));
        assert!(!system_prompt.content.contains("respond with `yield`"));
        assert!(!system_prompt.content.contains("current decision schema"));
        assert!(!system_prompt.content.contains("goal_reached"));
        assert!(!system_prompt.content.contains("cannot_proceed"));
    }

    #[tokio::test]
    async fn channel_assist_prompt_constants_resolve_managed_assets() {
        let storage = create_real_storage().await;
        let manager = PromptManager::new(Arc::new(storage));
        let prompts = [
            (
                prompt_names::CHANNEL_INGEST_DISTILL_SYSTEM,
                prompt_versions::CHANNEL_INGEST_DISTILL_SYSTEM_LEGACY,
            ),
            (
                prompt_names::CHANNEL_INGEST_DISTILL_USER,
                prompt_versions::CHANNEL_INGEST_DISTILL_USER_LEGACY,
            ),
            (
                prompt_names::CHANNEL_INGEST_DISTILL_SYSTEM,
                prompt_versions::CHANNEL_INGEST_DISTILL_SYSTEM,
            ),
            (
                prompt_names::CHANNEL_INGEST_DISTILL_USER,
                prompt_versions::CHANNEL_INGEST_DISTILL_USER,
            ),
            (
                prompt_names::CHANNEL_INGEST_DISTILL_REPAIR_USER,
                prompt_versions::CHANNEL_INGEST_DISTILL_REPAIR_USER,
            ),
            (
                prompt_names::CHANNEL_CLASSIFY_SYSTEM,
                prompt_versions::CHANNEL_CLASSIFY,
            ),
            (
                prompt_names::CHANNEL_CLASSIFY_USER,
                prompt_versions::CHANNEL_CLASSIFY,
            ),
            (
                prompt_names::CHANNEL_REPLY_DRAFT_SYSTEM,
                prompt_versions::CHANNEL_REPLY_DRAFT,
            ),
            (
                prompt_names::CHANNEL_REPLY_DRAFT_USER,
                prompt_versions::CHANNEL_REPLY_DRAFT,
            ),
            (
                prompt_names::CHANNEL_PATTERN_SYNTHESIS_SYSTEM,
                prompt_versions::CHANNEL_PATTERN_SYNTHESIS,
            ),
            (
                prompt_names::CHANNEL_PATTERN_SYNTHESIS_USER,
                prompt_versions::CHANNEL_PATTERN_SYNTHESIS,
            ),
            (
                prompt_names::RESURFACING_CURATE_SYSTEM,
                prompt_versions::RESURFACING_CURATE,
            ),
            (
                prompt_names::RESURFACING_CURATE_USER,
                prompt_versions::RESURFACING_CURATE,
            ),
            (
                prompt_names::RESURFACING_DEEP_SUMMARY_SYSTEM,
                prompt_versions::RESURFACING_DEEP_SUMMARY,
            ),
            (
                prompt_names::RESURFACING_DEEP_SUMMARY_USER,
                prompt_versions::RESURFACING_DEEP_SUMMARY,
            ),
        ];

        for (name, version) in prompts {
            let prompt = manager
                .get_prompt(name, version)
                .await
                .unwrap_or_else(|error| {
                    panic!("missing managed prompt {name} v{version}: {error}")
                });
            assert_eq!(prompt.name, name);
            assert_eq!(prompt.version, version);
        }
    }

    #[tokio::test]
    async fn memory_entity_prompts_keep_optional_fields_schema_bound() {
        let storage = create_real_storage().await;
        let manager = PromptManager::new(Arc::new(storage));
        let user = manager
            .get_prompt(
                prompt_names::MEMORY_EXTRACT_ENTITIES,
                prompt_versions::MEMORY_EXTRACT_ENTITIES,
            )
            .await
            .expect("current memory entity user prompt");
        let system = manager
            .get_prompt(
                prompt_names::MEMORY_EXTRACT_ENTITIES_SYSTEM,
                prompt_versions::MEMORY_EXTRACT_ENTITIES_SYSTEM,
            )
            .await
            .expect("current memory entity system prompt");

        assert_eq!(user.version, "1.2.0");
        assert_eq!(system.version, "1.2.0");
        assert!(user.content.contains("only fields declared"));
        assert!(user
            .content
            .contains("Emit `confidence`, source/provenance fields"));
        assert!(system
            .content
            .contains("Emit a `confidence` value only when"));
        assert!(!system.content.contains("Each entity must include"));
        assert!(!user
            .content
            .contains("Assign confidence scores based on evidence"));
    }

    #[tokio::test]
    async fn memory_distillation_prompts_keep_optional_fields_schema_bound() {
        let storage = create_real_storage().await;
        let manager = PromptManager::new(Arc::new(storage));
        let user = manager
            .get_prompt(
                prompt_names::MEMORY_DISTILL_INSIGHTS,
                prompt_versions::MEMORY_DISTILL_INSIGHTS,
            )
            .await
            .expect("current memory distillation user prompt");
        let system = manager
            .get_prompt(
                prompt_names::MEMORY_DISTILL_INSIGHTS_SYSTEM,
                prompt_versions::MEMORY_DISTILL_INSIGHTS_SYSTEM,
            )
            .await
            .expect("current memory distillation system prompt");

        assert_eq!(user.version, "1.1.0");
        assert_eq!(system.version, "1.1.0");
        assert!(user.content.contains("only through fields declared"));
        assert!(user
            .content
            .contains("otherwise do not emit `evidence_count`"));
        assert!(system
            .content
            .contains("only when that exact field is declared"));
        assert!(!user
            .content
            .contains("Sum evidence counts from merged insights"));
        assert!(!system
            .content
            .contains("Evidence counts should be summed when merging"));
    }

    #[tokio::test]
    async fn channel_assist_managed_prompts_render_with_runtime_variables() {
        let storage = create_real_storage().await;
        let manager = PromptManager::new(Arc::new(storage));

        let render = |name: &'static str, version: &'static str, variables: &[&'static str]| {
            let manager = &manager;
            let variables = variables
                .iter()
                .map(|name| ((*name).to_string(), "test".to_string()))
                .collect::<HashMap<_, _>>();
            async move {
                manager
                    .get_rendered_prompt(name, version, variables)
                    .await
                    .unwrap_or_else(|error| {
                        panic!("failed to render managed prompt {name} v{version}: {error}")
                    })
            }
        };

        let distill_variables = ["channel", "header_document", "content", "content_truncated"];
        render(
            prompt_names::CHANNEL_INGEST_DISTILL_USER,
            prompt_versions::CHANNEL_INGEST_DISTILL_USER_LEGACY,
            &distill_variables,
        )
        .await;
        render(
            prompt_names::CHANNEL_INGEST_DISTILL_USER,
            prompt_versions::CHANNEL_INGEST_DISTILL_USER,
            &distill_variables,
        )
        .await;
        render(
            prompt_names::CHANNEL_INGEST_DISTILL_REPAIR_USER,
            prompt_versions::CHANNEL_INGEST_DISTILL_REPAIR_USER,
            &["validation_error"],
        )
        .await;
        render(
            prompt_names::CHANNEL_CLASSIFY_USER,
            prompt_versions::CHANNEL_CLASSIFY,
            &[
                "channel",
                "lane",
                "subject",
                "sender",
                "recipient_domains",
                "label_ids",
                "message_count",
                "age",
                "latest_message_id",
                "latest_message_age",
                "latest_direction",
                "latest_intent",
                "needs_reply_hint",
                "follow_up_hint",
                "recent_handled_followups",
                "summary",
            ],
        )
        .await;
        render(
            prompt_names::CHANNEL_PATTERN_SYNTHESIS_USER,
            prompt_versions::CHANNEL_PATTERN_SYNTHESIS,
            &["digest"],
        )
        .await;
        render(
            prompt_names::RESURFACING_CURATE_USER,
            prompt_versions::RESURFACING_CURATE,
            &["candidates"],
        )
        .await;
        render(
            prompt_names::RESURFACING_DEEP_SUMMARY_USER,
            prompt_versions::RESURFACING_DEEP_SUMMARY,
            &[],
        )
        .await;
    }
}
