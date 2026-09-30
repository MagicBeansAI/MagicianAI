//! Shared helper for V2 Tool Matcher integration across strategies
//!
//! This module provides a unified interface for strategies to use V2 Tool
//! Matcher with fail-fast error handling (NO graceful degradation to V1).

use std::collections::HashMap;

use anyhow::Result;
use tracing::{debug, info, warn};

use super::types::{StrategyContext, CONFIDENCE_THRESHOLD};
use runtime_core::{SuccessMetrics, ToolMatch, ToolMatchResult, ToolMetadata};

use crate::magician_v2::tool_matcher::service::DEFAULT_SUCCESS_RATE;

/// Extended tool match result that includes delegation provenance from the V2 matcher.
///
/// The V1 `ToolMatchResult` (runtime-core) does not carry `providing_agent_id` because
/// it is a shared crate type. This wrapper augments the V1 result with the delegation
/// info needed by the planners to populate `PlanStep::providing_agent_id` and compute
/// `PlanStep::step_hash`.
#[derive(Debug, Clone)]
pub struct ToolMatchWithProvenance {
    /// The V1-compatible tool match result.
    pub result: ToolMatchResult,
    /// The agent that provides/owns the matched tool (`None` = self / no delegation).
    pub providing_agent_id: Option<String>,
}

/// Match tool using V2 Tool Matcher with fail-fast behavior
///
/// This helper function uses the V2 Tool Matcher if available in the context.
/// If V2 is unavailable, it falls back to V1 discovery.
/// If V2 is available but fails, it propagates the error (NO silent fallback).
///
/// # Arguments
/// * `context` - Strategy context containing tool discovery and optional V2
///   matcher
/// * `query` - Query string to match tools against
/// * `parent_task` - Optional parent task description for hierarchical context
/// * `parent_tool_name` - Optional parent tool name for context
///
/// # Returns
/// * `Result<ToolMatchWithProvenance>` - V2 result (converted to V1 format)
///   with delegation provenance, or V1 result with `providing_agent_id: None`
///
/// # Errors
/// * If V2 matcher is present but fails, error is propagated (fail-fast)
pub async fn match_tool_with_v2(
    context: &StrategyContext,
    query: &str,
    parent_task: Option<String>,
    parent_tool_name: Option<String>,
) -> Result<ToolMatchWithProvenance> {
    // If V2 Tool Matcher is available, use it with fail-fast behavior
    if let Some(ref v2_matcher) = context.v2_tool_matcher {
        debug!(
            "[MAGICIAN-V2-STRATEGY] Using V2 Tool Matcher with 4-tier progressive filtering \
             (fail-fast mode)"
        );
        debug!(
            "[MAGICIAN-V2-STRATEGY] 🔍 Context execution_id: {:?}, correlation_id: {:?}",
            context.execution_id, context.correlation_id
        );
        if parent_task.is_some() {
            debug!(
                "[MAGICIAN-V2-STRATEGY] 🔍 Parent context: task={:?}, tool={:?}",
                parent_task, parent_tool_name
            );
        }

        let match_request = crate::magician_v2::tool_matcher::ToolMatchRequest {
            task: query.to_string(),
            suggested_categories: context.suggested_categories.clone(),
            execution_context: context.execution_context.clone(),
            confidence_threshold: CONFIDENCE_THRESHOLD,
            execution_id: context.execution_id.clone(),
            correlation_id: context.correlation_id.clone(),
            required_capabilities: if context
                .query_analysis
                .dependencies
                .required_capabilities
                .is_empty()
            {
                None
            } else {
                Some(
                    context
                        .query_analysis
                        .dependencies
                        .required_capabilities
                        .clone(),
                )
            },
            parent_task,
            parent_tool_name,
        };

        debug!(
            "[MAGICIAN-V2-STRATEGY] 🔍 ToolMatchRequest execution_id: {:?}, correlation_id: {:?}",
            match_request.execution_id, match_request.correlation_id
        );

        // V2 explicitly required - handle NoConfidentMatches gracefully to allow
        // strategy escalation
        let v2_result = match v2_matcher.match_tool(&match_request).await {
            Ok(result) => result,
            Err(crate::magician_v2::tool_matcher::ToolMatchError::NoConfidentMatches {
                threshold,
                best_confidence,
            }) => {
                warn!(
                    "[MAGICIAN-V2-STRATEGY] 🔄 V2 Tool Matcher: No tools exceeded threshold \
                     {:.2}, best was {:.3}. Returning low-confidence result to allow strategy \
                     escalation",
                    threshold, best_confidence
                );

                // Return low-confidence result to allow orchestrator to escalate to next
                // strategy USER REQUIREMENT: Return actual best_confidence (not
                // 0.0) for proper escalation logic
                return Ok(ToolMatchWithProvenance {
                    result: ToolMatchResult {
                        primary_match: None,
                        match_confidence: best_confidence, // Actual confidence from V2 matcher
                        missing_capabilities: vec![],
                        parameter_coverage: 0.0,
                        executable: false,
                    },
                    providing_agent_id: None,
                });
            },
            Err(crate::magician_v2::tool_matcher::ToolMatchError::NoMatchingCategories {
                categories,
            }) => {
                warn!(
                    "[MAGICIAN-V2-STRATEGY] 🔄 V2 Tool Matcher: No tools match categories {:?}. \
                     Returning low-confidence result to allow strategy escalation",
                    categories
                );

                // Return low-confidence result to allow orchestrator to escalate to next
                // strategy Categories might be too specific or incorrectly
                // classified - let broader strategy try
                return Ok(ToolMatchWithProvenance {
                    result: ToolMatchResult {
                        primary_match: None,
                        match_confidence: 0.0,
                        missing_capabilities: vec![],
                        parameter_coverage: 0.0,
                        executable: false,
                    },
                    providing_agent_id: None,
                });
            },
            Err(e) => {
                // Other errors (security, LLM, etc.) should still fail fast
                return Err(anyhow::anyhow!("V2 Tool Matcher failed: {}", e));
            },
        };

        info!(
            "[MAGICIAN-V2-STRATEGY] V2 Tool Matcher: {} (confidence={:.3}) in {}ms",
            v2_result.tool_name, v2_result.confidence, v2_result.match_time_ms
        );

        // Convert V2 result to V1 ToolMatchResult format, preserving providing_agent_id
        let providing_agent_id = v2_result.providing_agent_id.clone();
        return Ok(ToolMatchWithProvenance {
            result: ToolMatchResult {
                primary_match: Some(ToolMatch {
                    tool_name: v2_result.tool_name.clone(),
                    capability_match: v2_result.tier_scores.semantic_score,
                    parameter_mapping: HashMap::new(), /* V2 doesn't provide ParameterMapping -
                                                        * strategies handle this */
                    execution_confidence: v2_result.confidence,
                    tool_metadata: ToolMetadata {
                        name: v2_result.tool_name.clone(),
                        description: v2_result.tool_metadata.description.clone(),
                        category: v2_result.tool_metadata.category.clone(),
                        typical_use_cases: v2_result.tool_metadata.use_cases.clone(),
                        input_schema: convert_parameters_to_schema(
                            &v2_result.tool_metadata.parameters,
                        ),
                        output_schema: serde_json::json!({}), // V2 doesn't track output schema
                        success_rate: DEFAULT_SUCCESS_RATE,
                        avg_execution_time: v2_result.match_time_ms as f32,
                        enhanced_description: v2_result.tool_metadata.enhanced_description.clone(),
                        keywords: v2_result.tool_metadata.keywords.clone(),
                        use_cases: v2_result.tool_metadata.use_cases.clone(),
                        confidence_score: v2_result.confidence,
                        success_metrics: SuccessMetrics {
                            success_rate: DEFAULT_SUCCESS_RATE,
                            avg_execution_time: v2_result.match_time_ms as f32,
                            reliability_score: v2_result.confidence,
                            last_updated: chrono::Utc::now().timestamp(),
                        },
                        last_updated: chrono::Utc::now().timestamp(),
                    },
                }),
                match_confidence: v2_result.confidence,
                missing_capabilities: vec![],
                parameter_coverage: 1.0, // Assume full coverage from V2 matcher
                executable: true,
            },
            providing_agent_id,
        });
    }

    // V1 discovery path (only if V2 not available)
    // V1 does not carry providing_agent_id — delegation provenance is unavailable.
    debug!(
        "[MAGICIAN-V2-STRATEGY] V2 Tool Matcher not available, using V1 tool discovery with \
         category filter: {:?}",
        context.suggested_categories
    );
    let result = context
        .tool_matching
        .best_match(query, &context.execution_context)
        .await;
    Ok(ToolMatchWithProvenance {
        result,
        providing_agent_id: None,
    })
}

/// Convert V2 parameters to JSON schema format
fn convert_parameters_to_schema(
    parameters: &[crate::magician_v2::tool_matcher::ToolParameter],
) -> serde_json::Value {
    let properties: serde_json::Map<String, serde_json::Value> = parameters
        .iter()
        .map(|param| {
            let prop = serde_json::json!({
                "type": param.param_type,
                "description": param.description,
            });
            (param.name.clone(), prop)
        })
        .collect();

    let required: Vec<String> = parameters
        .iter()
        .filter(|p| p.required)
        .map(|p| p.name.clone())
        .collect();

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}
