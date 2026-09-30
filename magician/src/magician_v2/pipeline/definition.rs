//! # Pipeline Definition
//!
//! YAML-driven pipeline schema and parser. A [`PipelineDefinition`] describes
//! the ordered stages an agent pipeline runs through, their inputs/outputs,
//! loop/retry behaviour, and arbitrary per-stage configuration.
//!
//! [`PipelineOverrides`] allows runtime callers to skip stages, merge
//! additional config, or replace retry policies without mutating the
//! original definition.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::artifact::ArtifactType;

// ---------------------------------------------------------------------------
// PipelineDefinition
// ---------------------------------------------------------------------------

/// Complete pipeline definition parsed from YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineDefinition {
    pub stages: Vec<StageDefinition>,
}

// ---------------------------------------------------------------------------
// StageDefinition
// ---------------------------------------------------------------------------

/// A single stage in the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageDefinition {
    pub name: String,
    pub agent: String,
    #[serde(default)]
    pub inputs: Vec<ArtifactType>,
    #[serde(default)]
    pub outputs: Vec<ArtifactType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_config: Option<LoopConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,
    #[serde(default)]
    pub config: HashMap<String, Value>,
}

// ---------------------------------------------------------------------------
// ConvergenceCriterion
// ---------------------------------------------------------------------------

/// Convergence criterion for a loop-config stage (M-13).
///
/// Replaces the previous `convergence: String` free-text field.
/// Unknown string values now produce a deserialization error instead of
/// silently applying a fail-open default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvergenceCriterion {
    /// Converged when all slots are resolved (`needs_clarification = false`).
    AllSlotsResolved,
    /// Converged when every configured stage has completed at least once.
    AllComplete,
    /// Converged when any configured stage has completed.
    AnyComplete,
    /// Converged when the fraction of resolved slots meets or exceeds the threshold.
    Threshold { fraction: f32 },
}

// ---------------------------------------------------------------------------
// LoopConfig
// ---------------------------------------------------------------------------

/// Loop/convergence configuration for a stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopConfig {
    pub convergence: ConvergenceCriterion,
    pub max_iterations: u32,
}

// ---------------------------------------------------------------------------
// RetryConfig
// ---------------------------------------------------------------------------

/// Retry configuration for a stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<String>,
}

// ---------------------------------------------------------------------------
// StageOverride
// ---------------------------------------------------------------------------

/// Per-stage override for applying runtime modifications.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StageOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<HashMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip: Option<bool>,
}

// ---------------------------------------------------------------------------
// PipelineOverrides
// ---------------------------------------------------------------------------

/// Runtime overrides applied to a pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PipelineOverrides {
    #[serde(default)]
    pub skip_stages: Vec<String>,
    #[serde(default)]
    pub stage_overrides: HashMap<String, StageOverride>,
}

// ---------------------------------------------------------------------------
// impl PipelineDefinition
// ---------------------------------------------------------------------------

impl PipelineDefinition {
    /// Apply runtime overrides, returning a new definition with modifications.
    ///
    /// Stages listed in `overrides.skip_stages` are removed entirely.
    /// Per-stage overrides can additionally skip a stage (`skip: true`),
    /// merge extra config keys, or replace the retry policy.
    pub fn apply_overrides(&self, overrides: &PipelineOverrides) -> PipelineDefinition {
        let stages = self
            .stages
            .iter()
            .filter(|stage| !overrides.skip_stages.contains(&stage.name))
            .filter(|stage| {
                // Filter out stages that have skip=true in overrides before doing any
                // config-merging work on them.
                if let Some(stage_override) = overrides.stage_overrides.get(&stage.name) {
                    stage_override.skip != Some(true)
                } else {
                    true
                }
            })
            .map(|stage| {
                if let Some(stage_override) = overrides.stage_overrides.get(&stage.name) {
                    let mut modified = stage.clone();
                    // Merge config
                    if let Some(ref override_config) = stage_override.config {
                        for (k, v) in override_config {
                            modified.config.insert(k.clone(), v.clone());
                        }
                    }
                    // Override retry
                    if let Some(ref retry) = stage_override.retry {
                        modified.retry = Some(retry.clone());
                    }
                    modified
                } else {
                    stage.clone()
                }
            })
            .collect();

        PipelineDefinition { stages }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn minimal_yaml() -> &'static str {
        r#"
stages:
  - name: query_analysis
    agent: "system:query-analyzer"
    outputs:
      - query_analysis
  - name: planning
    agent: "system:planner"
    inputs:
      - query_analysis
    outputs:
      - plan_graph
"#
    }

    fn full_yaml() -> &'static str {
        r#"
stages:
  - name: query_analysis
    agent: "system:query-analyzer"
    outputs:
      - query_analysis
    config:
      model: "fast"
  - name: slot_extraction
    agent: "system:slot-extractor"
    inputs:
      - query_analysis
    outputs:
      - slot_graph
  - name: elicitation
    agent: "system:elicitor"
    inputs:
      - slot_graph
    outputs:
      - elicitation_result
    loop_config:
      convergence: "all_slots_resolved"
      max_iterations: 3
  - name: planning
    agent: "system:planner"
    inputs:
      - elicitation_result
    outputs:
      - plan_graph
    retry:
      max_attempts: 2
      escalation: "fallback_strategy"
"#
    }

    #[test]
    fn minimal_pipeline_parses_from_yaml() {
        let def: PipelineDefinition = serde_yaml::from_str(minimal_yaml()).unwrap();
        assert_eq!(def.stages.len(), 2);
        assert_eq!(def.stages[0].name, "query_analysis");
        assert_eq!(def.stages[0].agent, "system:query-analyzer");
        assert_eq!(def.stages[1].name, "planning");
    }

    #[test]
    fn full_pipeline_parses_from_yaml() {
        let def: PipelineDefinition = serde_yaml::from_str(full_yaml()).unwrap();
        assert_eq!(def.stages.len(), 4);

        // Check loop config
        let elicitation = &def.stages[2];
        assert!(elicitation.loop_config.is_some());
        let loop_cfg = elicitation.loop_config.as_ref().unwrap();
        assert_eq!(loop_cfg.convergence, ConvergenceCriterion::AllSlotsResolved);
        assert_eq!(loop_cfg.max_iterations, 3);

        // Check retry config
        let planning = &def.stages[3];
        assert!(planning.retry.is_some());
        let retry = planning.retry.as_ref().unwrap();
        assert_eq!(retry.max_attempts, 2);
        assert_eq!(retry.escalation.as_deref(), Some("fallback_strategy"));

        // Check config map
        let qa = &def.stages[0];
        assert_eq!(
            qa.config.get("model").and_then(|v| v.as_str()),
            Some("fast")
        );
    }

    /// M-13: ConvergenceCriterion deserializes from snake_case strings.
    #[test]
    fn convergence_criterion_deserializes_from_snake_case() {
        let lc: LoopConfig =
            serde_yaml::from_str("convergence: all_slots_resolved\nmax_iterations: 3").unwrap();
        assert_eq!(lc.convergence, ConvergenceCriterion::AllSlotsResolved);

        let lc2: LoopConfig =
            serde_yaml::from_str("convergence: all_complete\nmax_iterations: 1").unwrap();
        assert_eq!(lc2.convergence, ConvergenceCriterion::AllComplete);

        let bad: Result<LoopConfig, _> =
            serde_yaml::from_str("convergence: unknown_string\nmax_iterations: 1");
        assert!(
            bad.is_err(),
            "unknown convergence string must fail deserialization"
        );
    }

    #[test]
    fn overrides_skip_named_stages() {
        let def: PipelineDefinition = serde_yaml::from_str(full_yaml()).unwrap();
        let overrides = PipelineOverrides {
            skip_stages: vec!["slot_extraction".to_string()],
            stage_overrides: HashMap::new(),
        };
        let result = def.apply_overrides(&overrides);
        assert_eq!(result.stages.len(), 3);
        assert!(result.stages.iter().all(|s| s.name != "slot_extraction"));
    }

    #[test]
    fn overrides_merge_stage_config() {
        let def: PipelineDefinition = serde_yaml::from_str(full_yaml()).unwrap();
        let mut stage_overrides = HashMap::new();
        let mut config_override = HashMap::new();
        config_override.insert("temperature".to_string(), serde_json::json!(0.5));
        stage_overrides.insert(
            "query_analysis".to_string(),
            StageOverride {
                config: Some(config_override),
                retry: None,
                skip: None,
            },
        );
        let overrides = PipelineOverrides {
            skip_stages: vec![],
            stage_overrides,
        };
        let result = def.apply_overrides(&overrides);
        let qa = &result.stages[0];
        // Original config preserved
        assert_eq!(
            qa.config.get("model").and_then(|v| v.as_str()),
            Some("fast")
        );
        // New config merged
        assert_eq!(
            qa.config.get("temperature").and_then(|v| v.as_f64()),
            Some(0.5)
        );
    }

    #[test]
    fn overrides_skip_via_stage_override() {
        let def: PipelineDefinition = serde_yaml::from_str(full_yaml()).unwrap();
        let mut stage_overrides = HashMap::new();
        stage_overrides.insert(
            "elicitation".to_string(),
            StageOverride {
                config: None,
                retry: None,
                skip: Some(true),
            },
        );
        let overrides = PipelineOverrides {
            skip_stages: vec![],
            stage_overrides,
        };
        let result = def.apply_overrides(&overrides);
        assert_eq!(result.stages.len(), 3);
        assert!(result.stages.iter().all(|s| s.name != "elicitation"));
    }

    #[test]
    fn overrides_replace_retry() {
        let def: PipelineDefinition = serde_yaml::from_str(full_yaml()).unwrap();
        let mut stage_overrides = HashMap::new();
        stage_overrides.insert(
            "planning".to_string(),
            StageOverride {
                config: None,
                retry: Some(RetryConfig {
                    max_attempts: 5,
                    escalation: Some("hard_fail".to_string()),
                }),
                skip: None,
            },
        );
        let overrides = PipelineOverrides {
            skip_stages: vec![],
            stage_overrides,
        };
        let result = def.apply_overrides(&overrides);
        let planning = result.stages.iter().find(|s| s.name == "planning").unwrap();
        let retry = planning.retry.as_ref().unwrap();
        assert_eq!(retry.max_attempts, 5);
        assert_eq!(retry.escalation.as_deref(), Some("hard_fail"));
    }

    #[test]
    fn yaml_serialize_deserialize_roundtrip() {
        let def: PipelineDefinition = serde_yaml::from_str(full_yaml()).unwrap();
        let yaml_out = serde_yaml::to_string(&def).unwrap();
        let roundtripped: PipelineDefinition = serde_yaml::from_str(&yaml_out).unwrap();
        assert_eq!(roundtripped.stages.len(), def.stages.len());
        for (orig, rt) in def.stages.iter().zip(roundtripped.stages.iter()) {
            assert_eq!(orig.name, rt.name);
            assert_eq!(orig.agent, rt.agent);
        }
    }
}
