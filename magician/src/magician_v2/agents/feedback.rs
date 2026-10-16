//! Phase 3 feedback loop scaffolding.

use std::borrow::Cow;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use tracing::warn;

use crate::magician_v2::artifact_v2::memory::V3EpisodeRecord;

use super::{memory::AgentMemoryService, types::FeedbackLoopDefinition};

#[derive(Debug, Clone, PartialEq)]
pub struct FeedbackSignal {
    pub loop_name: String,
    pub target: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Default)]
pub struct FeedbackLoopInterpreter;

impl FeedbackLoopInterpreter {
    /// Returns configured loops, or built-in defaults when none are provided.
    pub fn effective_loops<'a>(
        &self,
        configured: &'a [FeedbackLoopDefinition],
    ) -> Cow<'a, [FeedbackLoopDefinition]> {
        if configured.is_empty() {
            Cow::Owned(FeedbackLoopDefinition::defaults())
        } else {
            Cow::Borrowed(configured)
        }
    }

    pub fn run_loop_v3(
        &self,
        loop_def: &FeedbackLoopDefinition,
        episode: &V3EpisodeRecord,
    ) -> Option<FeedbackSignal> {
        if !trigger_matches(&loop_def.trigger, episode) {
            return None;
        }

        Some(FeedbackSignal {
            loop_name: loop_def.name.clone(),
            target: loop_def.inject_into.clone(),
            payload: json!({
                "source": loop_def.extract.source,
                "transform": loop_def.transform,
                "episode_id": episode.episode_id,
                "goal_id": episode.goal_id(),
                "outcome": episode.outcome_summary_text(),
            }),
        })
    }

    /// Runs all effective loops and returns produced feedback signals.
    pub fn run_effective_loops_v3(
        &self,
        configured: &[FeedbackLoopDefinition],
        episode: &V3EpisodeRecord,
    ) -> Vec<FeedbackSignal> {
        let loops = self.effective_loops(configured);
        loops
            .iter()
            .filter_map(|loop_def| self.run_loop_v3(loop_def, episode))
            .collect()
    }
}

fn trigger_matches(trigger: &str, episode: &V3EpisodeRecord) -> bool {
    match trigger.trim() {
        "episode.outcome.is_failed" => episode.outcome_is_failed(),
        "episode.outcome.is_succeeded" => episode.outcome_is_succeeded(),
        "episode.outcome.is_completed" => episode.outcome_is_completed(),
        // tactical pattern T3 — surface partial-success episodes to feedback loops.
        // `is_partial` matches the "almost succeeded" class (e.g. 4/5
        // questions answered). `is_failed_or_partial` is the recommended
        // failure_adaptation trigger because it covers BOTH hard failures
        // AND partial-success episodes — both are high-signal learning
        // surfaces. Authors writing new agent YAML should prefer the
        // composite trigger over bare `is_failed`.
        "episode.outcome.is_partial" => episode.outcome_is_partial(),
        "episode.outcome.is_failed_or_partial" => episode.outcome_is_failed_or_partial(),
        unknown => {
            warn!(
                trigger = unknown,
                "feedback_loop: unknown trigger expression — skipping; \
                 check for typos or wait for expression engine"
            );
            false
        },
    }
}

// ============================================================
// P6-10: FeedbackTransformer trait + registry
// ============================================================

/// A transformer that reads recent episode memory and produces text injected
/// into the next planning cycle's prompt. Built-in implementations are
/// registered in `FeedbackTransformerRegistry`.
pub trait FeedbackTransformer: Send + Sync {
    fn name(&self) -> &str;
    /// Execute the transform and return text to inject, or None if there is
    /// nothing to inject (e.g. no relevant episode history yet).
    fn apply(
        &self,
        signal: &FeedbackSignal,
        memory: &AgentMemoryService,
        agent_id: &str,
    ) -> BoxFuture<'_, anyhow::Result<Option<String>>>;
}

pub struct FailureContextTransformer;

impl FeedbackTransformer for FailureContextTransformer {
    fn name(&self) -> &str {
        "failure_context"
    }

    fn apply(
        &self,
        signal: &FeedbackSignal,
        memory: &AgentMemoryService,
        agent_id: &str,
    ) -> BoxFuture<'_, anyhow::Result<Option<String>>> {
        let goal_id = signal.payload["goal_id"].as_str().unwrap_or("").to_string();
        let agent_id = agent_id.to_string();
        let memory = memory.clone();

        Box::pin(async move {
            if goal_id.is_empty() {
                return Ok(None);
            }
            let episodes = memory
                .recall_native_episodes(&agent_id, &goal_id, None, Some(5))
                .await?;
            let failed: Vec<_> = episodes.iter().filter(|e| e.outcome_is_failed()).collect();
            if failed.is_empty() {
                return Ok(None);
            }
            let count = failed.len();
            let summaries: Vec<String> = failed
                .iter()
                .map(|e| e.outcome_error_summary())
                .filter(|s| !s.is_empty())
                .take(3)
                .collect();
            let text = format!(
                "This goal failed {} time{} recently. {}Try a different approach.",
                count,
                if count == 1 { "" } else { "s" },
                if summaries.is_empty() {
                    String::new()
                } else {
                    format!("Recent errors: {}. ", summaries.join("; "))
                }
            );
            Ok(Some(text))
        })
    }
}

pub struct SuccessPatternsTransformer;

impl FeedbackTransformer for SuccessPatternsTransformer {
    fn name(&self) -> &str {
        "success_patterns"
    }

    fn apply(
        &self,
        signal: &FeedbackSignal,
        memory: &AgentMemoryService,
        agent_id: &str,
    ) -> BoxFuture<'_, anyhow::Result<Option<String>>> {
        let goal_id = signal.payload["goal_id"].as_str().unwrap_or("").to_string();
        let agent_id = agent_id.to_string();
        let memory = memory.clone();

        Box::pin(async move {
            if goal_id.is_empty() {
                return Ok(None);
            }
            let episodes = memory
                .recall_native_episodes(&agent_id, &goal_id, None, Some(5))
                .await?;
            let strategies: Vec<&str> = episodes
                .iter()
                .filter(|e| e.outcome_is_succeeded())
                .filter_map(|e| e.strategy_summary.as_deref())
                .take(2)
                .collect();
            if strategies.is_empty() {
                return Ok(None);
            }
            let text = format!(
                "This goal succeeded recently. Successful approach: {}. Reuse if context is similar.",
                strategies.join("; ")
            );
            Ok(Some(text))
        })
    }
}

/// P6-10: Extracts recent strategy summaries from completed episodes for a goal
/// and formats them for injection into the next planning cycle.
pub struct RecordStrategyTransformer;

impl FeedbackTransformer for RecordStrategyTransformer {
    fn name(&self) -> &str {
        "record_strategy"
    }

    fn apply(
        &self,
        signal: &FeedbackSignal,
        memory: &AgentMemoryService,
        agent_id: &str,
    ) -> BoxFuture<'_, anyhow::Result<Option<String>>> {
        let goal_id = signal.payload["goal_id"].as_str().unwrap_or("").to_string();
        let agent_id = agent_id.to_string();
        let memory = memory.clone();

        Box::pin(async move {
            if goal_id.is_empty() {
                return Ok(None);
            }
            let episodes = memory
                .recall_native_episodes(&agent_id, &goal_id, None, Some(10))
                .await?;
            let completed: Vec<_> = episodes
                .iter()
                .filter(|e| e.outcome_is_completed())
                .filter_map(|e| {
                    e.strategy_summary
                        .as_deref()
                        .map(|s| (s, e.outcome_is_succeeded()))
                })
                .take(5)
                .collect();
            if completed.is_empty() {
                return Ok(None);
            }
            let summary = completed
                .iter()
                .map(|(s, ok)| format!("{} ({})", s, if *ok { "succeeded" } else { "failed" }))
                .collect::<Vec<_>>()
                .join("; ");
            Ok(Some(format!(
                "Recent strategies for this goal: {}",
                summary
            )))
        })
    }
}

/// Registry keyed by transformer name. Built-in transformers are pre-registered
/// via `with_builtins()`.
#[derive(Clone)]
pub struct FeedbackTransformerRegistry {
    transformers: std::collections::HashMap<String, std::sync::Arc<dyn FeedbackTransformer>>,
}

impl std::fmt::Debug for FeedbackTransformerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedbackTransformerRegistry")
            .field("transformer_count", &self.transformers.len())
            .finish()
    }
}

impl FeedbackTransformerRegistry {
    pub fn new() -> Self {
        Self {
            transformers: std::collections::HashMap::new(),
        }
    }

    /// Returns a registry pre-loaded with the built-in failure_context,
    /// success_patterns, and record_strategy transformers.
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        reg.register(std::sync::Arc::new(FailureContextTransformer));
        reg.register(std::sync::Arc::new(SuccessPatternsTransformer));
        reg.register(std::sync::Arc::new(RecordStrategyTransformer));
        reg
    }

    pub fn register(&mut self, t: std::sync::Arc<dyn FeedbackTransformer>) {
        self.transformers.insert(t.name().to_string(), t);
    }

    pub fn get(&self, name: &str) -> Option<&dyn FeedbackTransformer> {
        self.transformers.get(name).map(|t| t.as_ref())
    }

    /// Returns an iterator over all registered transformers.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &dyn FeedbackTransformer)> {
        self.transformers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_ref()))
    }
}

impl Default for FeedbackTransformerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::memory::EpisodeOutcome;
    use crate::magician_v2::agents::types::{FeedbackExtract, FeedbackLoopDefinition};
    use chrono::Utc;

    fn make_episode(
        agent_id: &str,
        goal_id: &str,
        seq: u64,
        episode_id: &str,
        outcome: EpisodeOutcome,
        strategy_summary: Option<&str>,
    ) -> V3EpisodeRecord {
        let now = Utc::now();
        V3EpisodeRecord::new_memory_episode(
            None,
            agent_id.to_string(),
            episode_id.to_string(),
            goal_id.to_string(),
            "manual".to_string(),
            seq,
            now,
            None,
            now,
            now,
            &outcome,
            vec![],
            vec![],
            vec![],
            strategy_summary.map(str::to_string),
            None,
            None,
        )
    }

    fn failed_episode() -> V3EpisodeRecord {
        make_episode(
            "a1",
            "g1",
            1,
            "e1",
            EpisodeOutcome::Failed {
                error: "boom".to_string(),
            },
            None,
        )
    }

    #[test]
    fn failed_trigger_produces_signal() {
        let loop_def = FeedbackLoopDefinition {
            name: "failure_adaptation".to_string(),
            trigger: "episode.outcome.is_failed".to_string(),
            extract: FeedbackExtract {
                source: "memory.episodes(goal_id, limit=5)".to_string(),
                filter: None,
                fields: vec![],
            },
            transform: "failure_context".to_string(),
            inject_into: "prompt.failure_context".to_string(),
        };

        let signal = FeedbackLoopInterpreter
            .run_loop_v3(&loop_def, &failed_episode())
            .expect("expected signal");
        assert_eq!(signal.loop_name, "failure_adaptation");
    }

    #[test]
    fn effective_loops_uses_defaults_when_config_empty() {
        let effective = FeedbackLoopInterpreter.effective_loops(&[]);
        assert_eq!(effective.len(), 3);
        assert_eq!(effective[0].name, "failure_adaptation");
        assert_eq!(effective[1].name, "success_reinforcement");
        assert_eq!(effective[2].name, "strategy_effectiveness");
    }

    #[test]
    fn run_effective_loops_uses_defaults_and_filters_by_trigger() {
        let signals = FeedbackLoopInterpreter.run_effective_loops_v3(&[], &failed_episode());
        // failure_adaptation (is_failed) + strategy_effectiveness (is_completed) both fire
        assert_eq!(signals.len(), 2);
        assert!(signals.iter().any(|s| s.loop_name == "failure_adaptation"));
        assert!(signals
            .iter()
            .any(|s| s.loop_name == "strategy_effectiveness"));
    }

    #[test]
    fn registry_with_builtins_has_failure_context_and_success_patterns() {
        let reg = FeedbackTransformerRegistry::with_builtins();
        assert!(reg.get("failure_context").is_some());
        assert!(reg.get("success_patterns").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn registry_register_and_get_roundtrip() {
        let mut reg = FeedbackTransformerRegistry::new();
        assert!(reg.get("failure_context").is_none());
        reg.register(std::sync::Arc::new(FailureContextTransformer));
        let t = reg.get("failure_context").expect("should be registered");
        assert_eq!(t.name(), "failure_context");
    }

    fn make_signal(goal_id: &str) -> FeedbackSignal {
        FeedbackSignal {
            loop_name: "failure_adaptation".to_string(),
            target: "failure_context".to_string(),
            payload: serde_json::json!({ "goal_id": goal_id }),
        }
    }

    fn make_failed_ep(agent_id: &str, goal_id: &str, seq: u64, ep_id: &str) -> V3EpisodeRecord {
        make_episode(
            agent_id,
            goal_id,
            seq,
            ep_id,
            EpisodeOutcome::Failed {
                error: "Tool X failed".to_string(),
            },
            None,
        )
    }

    fn make_success_ep(
        agent_id: &str,
        goal_id: &str,
        seq: u64,
        ep_id: &str,
        strategy: &str,
    ) -> V3EpisodeRecord {
        make_episode(
            agent_id,
            goal_id,
            seq,
            ep_id,
            EpisodeOutcome::GoalAchieved {
                summary: "done".to_string(),
            },
            Some(strategy),
        )
    }

    #[tokio::test]
    async fn failure_context_transformer_returns_text_for_failed_episodes() {
        use crate::magician_v2::agents::memory::AgentMemoryService;
        let tmp = tempfile::tempdir().unwrap();
        let mem = AgentMemoryService::with_base_path(tmp.path());

        let ep1 = make_failed_ep("agent1", "g1", 1, "ep1");
        let ep2 = make_failed_ep("agent1", "g1", 2, "ep2");
        mem.append_native_episode("agent1", &ep1).await.unwrap();
        mem.append_native_episode("agent1", &ep2).await.unwrap();

        let signal = make_signal("g1");
        let result = FailureContextTransformer
            .apply(&signal, &mem, "agent1")
            .await
            .unwrap();
        let text = result.expect("should produce text for failed episodes");
        assert!(
            text.contains("failed"),
            "text should mention failures: {text}"
        );
        assert!(text.contains("2"), "should count 2 failures: {text}");
    }

    #[tokio::test]
    async fn failure_context_transformer_returns_none_when_no_failures() {
        use crate::magician_v2::agents::memory::AgentMemoryService;
        let tmp = tempfile::tempdir().unwrap();
        let mem = AgentMemoryService::with_base_path(tmp.path());

        let signal = make_signal("g1");
        let result = FailureContextTransformer
            .apply(&signal, &mem, "agent1")
            .await
            .unwrap();
        assert!(result.is_none(), "no episodes means no text");
    }

    #[tokio::test]
    async fn success_patterns_transformer_returns_strategy_text() {
        use crate::magician_v2::agents::memory::AgentMemoryService;
        let tmp = tempfile::tempdir().unwrap();
        let mem = AgentMemoryService::with_base_path(tmp.path());

        let ep = make_success_ep("agent1", "g1", 1, "ep1", "use parallel search");
        mem.append_native_episode("agent1", &ep).await.unwrap();

        let signal = FeedbackSignal {
            loop_name: "success_reinforcement".to_string(),
            target: "success_patterns".to_string(),
            payload: serde_json::json!({ "goal_id": "g1" }),
        };
        let result = SuccessPatternsTransformer
            .apply(&signal, &mem, "agent1")
            .await
            .unwrap();
        let text = result.expect("should produce text for succeeded episodes");
        assert!(
            text.contains("use parallel search"),
            "text should include strategy: {text}"
        );
    }

    #[tokio::test]
    async fn success_patterns_transformer_returns_none_when_no_strategy() {
        use crate::magician_v2::agents::memory::AgentMemoryService;
        let tmp = tempfile::tempdir().unwrap();
        let mem = AgentMemoryService::with_base_path(tmp.path());

        // Succeeded but no strategy_summary
        let ep = make_failed_ep("agent1", "g1", 1, "ep1"); // only failure, no strategy
        mem.append_native_episode("agent1", &ep).await.unwrap();

        let signal = FeedbackSignal {
            loop_name: "success_reinforcement".to_string(),
            target: "success_patterns".to_string(),
            payload: serde_json::json!({ "goal_id": "g1" }),
        };
        let result = SuccessPatternsTransformer
            .apply(&signal, &mem, "agent1")
            .await
            .unwrap();
        assert!(result.is_none(), "no succeeded episodes means no text");
    }
}
