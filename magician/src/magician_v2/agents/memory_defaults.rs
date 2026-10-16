//! Default memory configuration for autonomous personal agents.
//!
//! When a personal agent has `autonomous_config` but no explicit memory tiers or
//! consolidation rules, this module provides sensible defaults that cover entity
//! extraction, insight distillation, activity tracking, archival, and user-level
//! promotion.

use std::collections::{BTreeMap, HashMap};

use crate::magician_v2::{
    llm_chunking::MAX_ARCHIVE_GROUP_EPISODES,
    prompts::constants::{names as prompt_names, versions as prompt_versions},
};

use super::memory_tiers::{
    BuiltinTransform, ConsolidationTransform, ConsolidationTrigger, MemoryConsolidationOperation,
    MemoryConsolidationRule, MemoryTierDefinition, MergeStrategy, RenderConfig, RetentionMode,
    TierFieldSchema, TierScope,
};
use super::types::EpisodeRetention;

const DEFAULT_DURABLE_COLLECTION_MAX_ITEMS: usize = 1_000;

/// Returns the default memory configuration for a personal agent with autonomous
/// capabilities.
///
/// The returned tuple contains:
/// - 6 memory tier definitions (entities, insights, recent_activity, archive, task_progress, environment_knowledge)
/// - 9 consolidation rules covering extraction, tracking, distillation, archival,
///   expiry, promotion, and environment knowledge extraction
/// - Episode retention configured for 7-day window with consolidate-before-delete
pub fn default_memory_config_for_personal_agent() -> (
    Vec<MemoryTierDefinition>,
    Vec<MemoryConsolidationRule>,
    EpisodeRetention,
) {
    let tiers = default_tiers();
    let rules = default_consolidation_rules();
    let retention = default_episode_retention();
    (tiers, rules, retention)
}

fn default_tiers() -> Vec<MemoryTierDefinition> {
    vec![
        // 1. entities — extracted people, places, projects, etc.
        MemoryTierDefinition {
            name: "entities".to_string(),
            scope: TierScope::Agent,
            description:
                "Extracted entities (people, places, projects) with attributes and recency tracking"
                    .to_string(),
            schema: BTreeMap::from([(
                "entities".to_string(),
                TierFieldSchema::Collection {
                    max_items: Some(DEFAULT_DURABLE_COLLECTION_MAX_ITEMS),
                    item_schema: Some(BTreeMap::from([
                        ("name".to_string(), TierFieldSchema::Text {}),
                        ("type".to_string(), TierFieldSchema::Text {}),
                        ("attributes".to_string(), TierFieldSchema::KeyValueList {}),
                        ("last_seen".to_string(), TierFieldSchema::DateTime {}),
                    ])),
                },
            )]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{entities}".to_string(),
            },
            retention: RetentionMode::Forever,
        },
        // 2. insights — distilled patterns and observations
        MemoryTierDefinition {
            name: "insights".to_string(),
            scope: TierScope::Agent,
            description: "Distilled insights and patterns with confidence scores and provenance"
                .to_string(),
            schema: BTreeMap::from([(
                "insights".to_string(),
                TierFieldSchema::Collection {
                    max_items: Some(100),
                    item_schema: Some(BTreeMap::from([
                        ("pattern".to_string(), TierFieldSchema::Text {}),
                        ("type".to_string(), TierFieldSchema::Text {}),
                        ("confidence".to_string(), TierFieldSchema::Text {}),
                        ("evidence_count".to_string(), TierFieldSchema::Text {}),
                        ("staleness_risk".to_string(), TierFieldSchema::Text {}),
                        (
                            "source_episodes".to_string(),
                            TierFieldSchema::KeyValueList {},
                        ),
                        ("merged_from".to_string(), TierFieldSchema::KeyValueList {}),
                        ("created_at".to_string(), TierFieldSchema::DateTime {}),
                    ])),
                },
            )]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{insights}".to_string(),
            },
            retention: RetentionMode::Forever,
        },
        // 3. recent_activity — agent-scoped summary of the latest activity
        MemoryTierDefinition {
            name: "recent_activity".to_string(),
            scope: TierScope::Agent,
            description: "Agent-scoped summary of recent activity, tools used, and outcome"
                .to_string(),
            schema: BTreeMap::from([
                ("summary".to_string(), TierFieldSchema::Text {}),
                ("period".to_string(), TierFieldSchema::Document {}),
                (
                    "key_actions".to_string(),
                    TierFieldSchema::Collection {
                        max_items: Some(10),
                        item_schema: None,
                    },
                ),
                (
                    "tools_used".to_string(),
                    TierFieldSchema::Collection {
                        max_items: Some(20),
                        item_schema: None,
                    },
                ),
                (
                    "topics".to_string(),
                    TierFieldSchema::Collection {
                        max_items: Some(20),
                        item_schema: None,
                    },
                ),
                ("outcome_status".to_string(), TierFieldSchema::Text {}),
            ]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{summary}".to_string(),
            },
            retention: RetentionMode::Days(30),
        },
        // 4. archive — long-term summaries of past activity
        MemoryTierDefinition {
            name: "archive".to_string(),
            scope: TierScope::Agent,
            description: "Long-term archived summaries of past activity periods".to_string(),
            schema: BTreeMap::from([(
                "summaries".to_string(),
                TierFieldSchema::Collection {
                    max_items: Some(500),
                    item_schema: Some(BTreeMap::from([
                        ("period".to_string(), TierFieldSchema::Text {}),
                        ("summary".to_string(), TierFieldSchema::Document {}),
                        ("key_events".to_string(), TierFieldSchema::KeyValueList {}),
                        (
                            "entity_mentions".to_string(),
                            TierFieldSchema::KeyValueList {},
                        ),
                        (
                            "source_episode_ids".to_string(),
                            TierFieldSchema::Collection {
                                max_items: Some(MAX_ARCHIVE_GROUP_EPISODES),
                                item_schema: None,
                            },
                        ),
                    ])),
                },
            )]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{summaries}".to_string(),
            },
            retention: RetentionMode::Forever,
        },
        // 5. task_progress — active goal tracking
        MemoryTierDefinition {
            name: "task_progress".to_string(),
            scope: TierScope::AgentGoal,
            description: "Per-goal progress tracking with status, notes, and pending items"
                .to_string(),
            schema: BTreeMap::from([
                ("goal_id".to_string(), TierFieldSchema::Text {}),
                ("status".to_string(), TierFieldSchema::Text {}),
                ("first_seen".to_string(), TierFieldSchema::DateTime {}),
                ("context_summary".to_string(), TierFieldSchema::Text {}),
                (
                    "items".to_string(),
                    TierFieldSchema::Collection {
                        max_items: Some(50),
                        item_schema: Some(BTreeMap::from([
                            ("id".to_string(), TierFieldSchema::Text {}),
                            ("description".to_string(), TierFieldSchema::Text {}),
                            ("status".to_string(), TierFieldSchema::Text {}),
                        ])),
                    },
                ),
                (
                    "notes".to_string(),
                    TierFieldSchema::Collection {
                        max_items: Some(20),
                        item_schema: None,
                    },
                ),
            ]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{context_summary}".to_string(),
            },
            retention: RetentionMode::GoalLifetime,
        },
        // 6. environment_knowledge — learned knowledge about websites, APIs, CLI tools
        MemoryTierDefinition {
            name: "environment_knowledge".to_string(),
            scope: TierScope::Agent,
            description: "Learned knowledge about websites, APIs, CLI tools, and environments"
                .to_string(),
            schema: BTreeMap::from([(
                "environments".to_string(),
                TierFieldSchema::Collection {
                    max_items: Some(500),
                    item_schema: Some(BTreeMap::from([
                        ("name".to_string(), TierFieldSchema::Text {}),
                        ("environment_key".to_string(), TierFieldSchema::Text {}),
                        ("kind".to_string(), TierFieldSchema::Text {}),
                        ("page_type".to_string(), TierFieldSchema::Text {}),
                        ("layout_notes".to_string(), TierFieldSchema::Text {}),
                        (
                            "known_blockers".to_string(),
                            TierFieldSchema::KeyValueList {},
                        ),
                        ("successful_patterns".to_string(), TierFieldSchema::Text {}),
                        ("failure_modes".to_string(), TierFieldSchema::Text {}),
                        ("auth_required".to_string(), TierFieldSchema::Text {}),
                        ("last_used".to_string(), TierFieldSchema::DateTime {}),
                        ("use_count".to_string(), TierFieldSchema::Text {}),
                    ])),
                },
            )]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{environments}".to_string(),
            },
            retention: RetentionMode::Forever,
        },
        // NOTE: code knowledge (M3 `code_knowledge`) is intentionally NOT a default
        // tier. The coding agents are `kind: worker` (which skip these
        // personal-agent defaults) and carry their OWN `codebase_knowledge` /
        // `architectural_knowledge` tiers — all of which infer into the single
        // `CodeKnowledge` lane. A default producer here would be wrong
        // (default-config agents don't run code). The lane + `code_knowledge`
        // infer-keyword coverage live in magician-vector-index; see
        // docs/plans/2026-06-15-m3-code-knowledge.md.
    ]
}

fn default_consolidation_rules() -> Vec<MemoryConsolidationRule> {
    vec![
        // 1. extract_entities — after each step, extract entities from episodes
        MemoryConsolidationRule {
            name: "extract_entities".to_string(),
            trigger: ConsolidationTrigger::StepCompleted,
            source: "episodes(unprocessed=true)".to_string(),
            target: "entities".to_string(),
            transform: ConsolidationTransform::Llm {
                operation: Some(MemoryConsolidationOperation::MemoryEntityExtraction),
                prompt: prompt_ref(
                    prompt_names::MEMORY_EXTRACT_ENTITIES,
                    prompt_versions::MEMORY_EXTRACT_ENTITIES,
                ),
                system_prompt: Some(prompt_ref(
                    prompt_names::MEMORY_EXTRACT_ENTITIES_SYSTEM,
                    prompt_versions::MEMORY_EXTRACT_ENTITIES_SYSTEM,
                )),
                merge: Some(MergeStrategy::UpsertByName),
            },
        },
        // 2. extract_insights — after each step, extract insights from episodes
        MemoryConsolidationRule {
            name: "extract_insights".to_string(),
            trigger: ConsolidationTrigger::StepCompleted,
            source: "episodes(unprocessed=true)".to_string(),
            target: "insights".to_string(),
            transform: ConsolidationTransform::Llm {
                operation: Some(MemoryConsolidationOperation::MemoryInsightDistillation),
                prompt: "$ref:memory_extract_insights:1.0.0".to_string(),
                system_prompt: Some("$ref:memory_extract_insights_system:1.0.0".to_string()),
                merge: Some(MergeStrategy::UpsertByName),
            },
        },
        // 3. summarize_recent_activity — periodically summarize agent activity
        MemoryConsolidationRule {
            name: "summarize_recent_activity".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: None,
                interval_days: None,
                min_episodes: Some(10),
                max_staleness_hours: Some(24),
            },
            source: "episodes(unprocessed=true)".to_string(),
            target: "recent_activity".to_string(),
            transform: ConsolidationTransform::Llm {
                operation: Some(MemoryConsolidationOperation::MemoryArchiveSummary),
                prompt: "$ref:memory_summarize_activity:1.0.0".to_string(),
                system_prompt: Some("$ref:memory_summarize_activity_system:1.0.0".to_string()),
                merge: None,
            },
        },
        // 4. update_task_progress — after each step, track goal progress
        MemoryConsolidationRule {
            name: "update_task_progress".to_string(),
            trigger: ConsolidationTrigger::StepCompleted,
            source: "episodes(unprocessed=true)".to_string(),
            target: "task_progress".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        },
        // 5. distill_insights — periodic insight distillation
        MemoryConsolidationRule {
            name: "distill_insights".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: Some(24),
                interval_days: None,
                min_episodes: None, // min_episodes only valid for episodes(...) sources
                max_staleness_hours: Some(72),
            },
            source: "tiers(insights)".to_string(),
            target: "insights".to_string(),
            transform: ConsolidationTransform::Llm {
                operation: Some(MemoryConsolidationOperation::MemoryInsightDistillation),
                prompt: prompt_ref(
                    prompt_names::MEMORY_DISTILL_INSIGHTS,
                    prompt_versions::MEMORY_DISTILL_INSIGHTS,
                ),
                system_prompt: Some(prompt_ref(
                    prompt_names::MEMORY_DISTILL_INSIGHTS_SYSTEM,
                    prompt_versions::MEMORY_DISTILL_INSIGHTS_SYSTEM,
                )),
                merge: Some(MergeStrategy::UpsertBySimilarity),
            },
        },
        // 6. archive_old_episodes — periodic archival of old episodes
        MemoryConsolidationRule {
            name: "archive_old_episodes".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: Some(48),
                interval_days: None,
                min_episodes: Some(20),
                max_staleness_hours: Some(168),
            },
            source: "episodes(unprocessed=true)".to_string(),
            target: "archive".to_string(),
            transform: ConsolidationTransform::Llm {
                operation: Some(MemoryConsolidationOperation::MemoryArchiveSummary),
                prompt: "$ref:memory_archive_episodes:1.0.0".to_string(),
                system_prompt: Some("$ref:memory_archive_episodes_system:1.0.0".to_string()),
                merge: Some(MergeStrategy::AppendPeriod),
            },
        },
        // 7. expire_old_episodes — retention-based episode cleanup
        MemoryConsolidationRule {
            name: "expire_old_episodes".to_string(),
            trigger: ConsolidationTrigger::RetentionExpiry,
            source: "episodes(unprocessed=false)".to_string(),
            target: "archive".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::AppendArchiveSummary,
            },
        },
        // 8. extract_environment_knowledge — periodically extract environment knowledge
        MemoryConsolidationRule {
            name: "extract_environment_knowledge".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: None,
                interval_days: None,
                min_episodes: Some(10),
                max_staleness_hours: Some(24),
            },
            source: "episodes(unprocessed=true)".to_string(),
            target: "environment_knowledge".to_string(),
            transform: ConsolidationTransform::Llm {
                operation: Some(MemoryConsolidationOperation::MemoryEnvironmentKnowledgeExtraction),
                prompt: prompt_ref(
                    prompt_names::MEMORY_EXTRACT_ENVIRONMENT_KNOWLEDGE,
                    prompt_versions::MEMORY_EXTRACT_ENVIRONMENT_KNOWLEDGE,
                ),
                system_prompt: Some(prompt_ref(
                    prompt_names::MEMORY_EXTRACT_ENVIRONMENT_KNOWLEDGE_SYSTEM,
                    prompt_versions::MEMORY_EXTRACT_ENVIRONMENT_KNOWLEDGE_SYSTEM,
                )),
                merge: Some(MergeStrategy::UpsertByName),
            },
        },
        // 9. promote_to_user — periodically promote high-confidence insights to user profile
        MemoryConsolidationRule {
            name: "promote_to_user".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: Some(72),
                interval_days: None,
                min_episodes: None, // min_episodes only valid for episodes(...) sources
                max_staleness_hours: Some(168),
            },
            source: "tiers(insights)".to_string(),
            target: "user.preferences".to_string(),
            transform: ConsolidationTransform::Llm {
                operation: Some(MemoryConsolidationOperation::MemoryUserPromotion),
                prompt: prompt_ref(
                    prompt_names::MEMORY_PROMOTE_TO_USER,
                    prompt_versions::MEMORY_PROMOTE_TO_USER,
                ),
                system_prompt: Some(prompt_ref(
                    prompt_names::MEMORY_PROMOTE_TO_USER_SYSTEM,
                    prompt_versions::MEMORY_PROMOTE_TO_USER_SYSTEM,
                )),
                merge: Some(MergeStrategy::UpsertByName),
            },
        },
    ]
}

fn prompt_ref(name: &str, version: &str) -> String {
    format!("$ref:{name}:{version}")
}

fn default_episode_retention() -> EpisodeRetention {
    EpisodeRetention {
        default_days: 7, // 168 hours = 7 days
        on_failure: None,
        per_goal_override: HashMap::new(),
        consolidate_before_delete: true,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_has_6_tiers() {
        let (tiers, _, _) = default_memory_config_for_personal_agent();
        assert_eq!(tiers.len(), 6);
        let tier_names: Vec<&str> = tiers.iter().map(|t| t.name.as_str()).collect();
        assert!(tier_names.contains(&"entities"));
        assert!(tier_names.contains(&"insights"));
        assert!(tier_names.contains(&"recent_activity"));
        assert!(tier_names.contains(&"archive"));
        assert!(tier_names.contains(&"task_progress"));
        assert!(tier_names.contains(&"environment_knowledge"));
        // code knowledge is NOT a default tier — coding `kind: worker` agents
        // carry their own codebase_knowledge / architectural_knowledge tiers.
        assert!(!tier_names.contains(&"code_knowledge"));
    }

    #[test]
    fn test_default_config_has_9_rules() {
        let (_, rules, _) = default_memory_config_for_personal_agent();
        assert_eq!(rules.len(), 9);
        let rule_names: Vec<&str> = rules.iter().map(|r| r.name.as_str()).collect();
        assert!(rule_names.contains(&"extract_entities"));
        assert!(rule_names.contains(&"extract_insights"));
        assert!(rule_names.contains(&"summarize_recent_activity"));
        assert!(rule_names.contains(&"update_task_progress"));
        assert!(rule_names.contains(&"distill_insights"));
        assert!(rule_names.contains(&"archive_old_episodes"));
        assert!(rule_names.contains(&"expire_old_episodes"));
        assert!(rule_names.contains(&"extract_environment_knowledge"));
        assert!(rule_names.contains(&"promote_to_user"));
        let expire = rules
            .iter()
            .find(|rule| rule.name == "expire_old_episodes")
            .expect("expire rule");
        assert!(matches!(
            &expire.transform,
            ConsolidationTransform::Structured {
                builtin: BuiltinTransform::AppendArchiveSummary
            }
        ));
    }

    #[test]
    fn default_insight_contract_preserves_promotion_evidence_and_current_prompts() {
        let (tiers, rules, _) = default_memory_config_for_personal_agent();
        let insights = tiers
            .iter()
            .find(|tier| tier.name == "insights")
            .expect("default insights tier");
        let TierFieldSchema::Collection {
            item_schema: Some(item_schema),
            ..
        } = insights
            .schema
            .get("insights")
            .expect("insights collection")
        else {
            panic!("insights must declare a collection item schema");
        };
        for field in [
            "pattern",
            "type",
            "confidence",
            "evidence_count",
            "staleness_risk",
            "source_episodes",
            "merged_from",
            "created_at",
        ] {
            assert!(item_schema.contains_key(field), "missing `{field}`");
        }

        let distill = rules
            .iter()
            .find(|rule| rule.name == "distill_insights")
            .expect("distill insights rule");
        let ConsolidationTransform::Llm {
            prompt,
            system_prompt,
            ..
        } = &distill.transform
        else {
            panic!("distill insights must remain an LLM transform");
        };
        assert_eq!(
            prompt,
            &prompt_ref(
                prompt_names::MEMORY_DISTILL_INSIGHTS,
                prompt_versions::MEMORY_DISTILL_INSIGHTS
            )
        );
        let expected_system_prompt = prompt_ref(
            prompt_names::MEMORY_DISTILL_INSIGHTS_SYSTEM,
            prompt_versions::MEMORY_DISTILL_INSIGHTS_SYSTEM,
        );
        assert_eq!(
            system_prompt.as_deref(),
            Some(expected_system_prompt.as_str())
        );
    }

    #[test]
    fn default_archive_contract_carries_runtime_replay_identity() {
        let (tiers, _, _) = default_memory_config_for_personal_agent();
        let archive = tiers
            .iter()
            .find(|tier| tier.name == "archive")
            .expect("default archive tier");
        let TierFieldSchema::Collection {
            item_schema: Some(item_schema),
            ..
        } = archive.schema.get("summaries").expect("archive summaries")
        else {
            panic!("archive summaries must be a structured collection");
        };
        assert!(matches!(
            item_schema.get("source_episode_ids"),
            Some(TierFieldSchema::Collection {
                max_items: Some(MAX_ARCHIVE_GROUP_EPISODES),
                item_schema: None,
            })
        ));
    }

    #[test]
    fn test_default_config_retention() {
        let (_, _, retention) = default_memory_config_for_personal_agent();
        assert!(retention.consolidate_before_delete);
        assert_eq!(retention.default_days, 7);
    }

    #[test]
    fn test_default_entities_tier_schema() {
        let (tiers, _, _) = default_memory_config_for_personal_agent();
        let entities_tier = tiers.iter().find(|t| t.name == "entities").unwrap();

        assert!(matches!(entities_tier.scope, TierScope::Agent));
        assert!(entities_tier.schema.contains_key("entities"));

        let entities_field = entities_tier.schema.get("entities").unwrap();
        match entities_field {
            TierFieldSchema::Collection {
                max_items,
                item_schema,
            } => {
                assert_eq!(*max_items, Some(DEFAULT_DURABLE_COLLECTION_MAX_ITEMS));
                let item = item_schema.as_ref().unwrap();
                assert!(item.contains_key("name"));
                assert!(item.contains_key("type"));
                assert!(item.contains_key("attributes"));
                assert!(item.contains_key("last_seen"));
            },
            other => panic!("expected Collection, got {:?}", other),
        }
    }

    #[test]
    fn test_default_task_progress_tier_schema_matches_goal_tracking_shape() {
        let (tiers, rules, _) = default_memory_config_for_personal_agent();
        let task_progress_tier = tiers.iter().find(|t| t.name == "task_progress").unwrap();

        assert!(matches!(task_progress_tier.scope, TierScope::AgentGoal));
        assert!(matches!(
            task_progress_tier.retention,
            RetentionMode::GoalLifetime
        ));
        assert_eq!(task_progress_tier.render.template, "{context_summary}");
        assert!(task_progress_tier.schema.contains_key("goal_id"));
        assert!(task_progress_tier.schema.contains_key("status"));
        assert!(task_progress_tier.schema.contains_key("first_seen"));
        assert!(task_progress_tier.schema.contains_key("context_summary"));
        assert!(task_progress_tier.schema.contains_key("items"));
        assert!(task_progress_tier.schema.contains_key("notes"));

        let items_field = task_progress_tier.schema.get("items").unwrap();
        match items_field {
            TierFieldSchema::Collection {
                max_items,
                item_schema,
            } => {
                assert_eq!(*max_items, Some(50));
                let item = item_schema.as_ref().unwrap();
                assert!(item.contains_key("id"));
                assert!(item.contains_key("description"));
                assert!(item.contains_key("status"));
            },
            other => panic!("expected Collection, got {:?}", other),
        }

        let rule = rules
            .iter()
            .find(|rule| rule.name == "update_task_progress")
            .unwrap();
        assert_eq!(rule.target, "task_progress");
        assert!(matches!(
            rule.transform,
            ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask
            }
        ));
    }

    #[test]
    fn test_default_recent_activity_tier_has_cycle_summary_shape() {
        let (tiers, rules, _) = default_memory_config_for_personal_agent();
        let recent_activity_tier = tiers.iter().find(|t| t.name == "recent_activity").unwrap();

        assert!(matches!(recent_activity_tier.scope, TierScope::Agent));
        assert_eq!(recent_activity_tier.render.template, "{summary}");
        assert!(matches!(
            recent_activity_tier.retention,
            RetentionMode::Days(30)
        ));
        assert!(recent_activity_tier.schema.contains_key("summary"));
        assert!(recent_activity_tier.schema.contains_key("period"));
        assert!(recent_activity_tier.schema.contains_key("key_actions"));
        assert!(recent_activity_tier.schema.contains_key("tools_used"));
        assert!(recent_activity_tier.schema.contains_key("topics"));
        assert!(recent_activity_tier.schema.contains_key("outcome_status"));

        let rule = rules
            .iter()
            .find(|rule| rule.name == "summarize_recent_activity")
            .unwrap();
        assert_eq!(rule.target, "recent_activity");
        assert!(matches!(
            rule.trigger,
            ConsolidationTrigger::Batch {
                min_episodes: Some(10),
                max_staleness_hours: Some(24),
                ..
            }
        ));
        match &rule.transform {
            ConsolidationTransform::Llm {
                prompt,
                system_prompt,
                merge,
                ..
            } => {
                assert_eq!(prompt, "$ref:memory_summarize_activity:1.0.0");
                assert_eq!(
                    system_prompt.as_deref(),
                    Some("$ref:memory_summarize_activity_system:1.0.0")
                );
                assert!(merge.is_none());
            },
            other => panic!("expected Llm transform, got {:?}", other),
        }
    }

    #[test]
    fn test_default_rules_use_ref_prompts() {
        let (_, rules, _) = default_memory_config_for_personal_agent();
        for rule in &rules {
            match &rule.transform {
                ConsolidationTransform::Llm {
                    prompt,
                    system_prompt,
                    ..
                } => {
                    assert!(
                        prompt.starts_with("$ref:"),
                        "rule `{}` prompt should start with $ref:, got: {}",
                        rule.name,
                        prompt
                    );
                    if let Some(sp) = system_prompt {
                        assert!(
                            sp.starts_with("$ref:"),
                            "rule `{}` system_prompt should start with $ref:, got: {}",
                            rule.name,
                            sp
                        );
                    }
                },
                ConsolidationTransform::Structured { .. } => {
                    // Structured transforms don't have prompts — skip.
                },
                ConsolidationTransform::Render { .. } => {
                    // Render transforms don't have prompts — skip.
                },
            }
        }

        // Verify that at least the LLM rules all use $ref: prompts.
        let llm_rules: Vec<_> = rules
            .iter()
            .filter(|r| matches!(r.transform, ConsolidationTransform::Llm { .. }))
            .collect();
        assert_eq!(llm_rules.len(), 7, "expected 7 LLM rules in default config");
    }
}
