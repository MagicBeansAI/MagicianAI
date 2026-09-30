// Unified Query Analyzer: Single LLM call for comprehensive query analysis

use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use magicllm::error::LLMError;
use reqwest::Error as ReqwestError;
use serde::{Deserialize, Serialize};
use tokio::time::error::Elapsed;
use tracing::{debug, error, info, warn};

use crate::magician_v2::{
    analytics::operation_llm_telemetry::{
        OperationLlmCallAttribution, OperationLlmTelemetryContext,
    },
    prompt_identity::render_prompt_identity_from_metadata,
    prompts::{
        constants::{names, versions},
        PromptManager,
    },
    query_analysis::{
        intent::{QueryIntent, SlotMatchAnalysis},
        operation_llm_router::QueryAnalysisLLM,
    },
    storage::{TurnDirection, V2Slot, V2Turn, WaitingState},
};
use runtime_core::{ExecutionContext, ToolCatalog, ToolInfo};

/// Conversation context for intent-aware analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationContext {
    /// Current execution waiting state
    pub execution_status: WaitingState,

    /// Pending slots waiting for user input
    pub pending_slots: Vec<V2Slot>,

    /// Recent conversation history (last 3-5 turns)
    pub recent_turns: Vec<V2Turn>,
}

/// Entities extracted from query text during analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntities {
    /// Key-value pairs extracted from query
    /// Example: "ping google.com" → {"target": "google.com"}
    pub entities: HashMap<String, String>,

    /// Named entities grouped by type
    /// Example: {"hostname": ["google.com"], "port": ["443"], "path":
    /// ["/api/v1"]}
    pub typed_entities: HashMap<String, Vec<String>>,

    /// Confidence in extraction accuracy (0.0-1.0)
    pub extraction_confidence: f32,

    /// Explanation of what was extracted and why
    pub extraction_reasoning: String,
}

impl Default for ExtractedEntities {
    fn default() -> Self {
        Self {
            entities: HashMap::new(),
            typed_entities: HashMap::new(),
            extraction_confidence: 0.0,
            extraction_reasoning: String::new(),
        }
    }
}

/// Task clarity classification for staged planning.
///
/// The staged planning pipeline now always runs through intent classification,
/// slot extraction, elicitation, query rewrite, and planning. `TaskClarity`
/// remains useful as a planning signal: more concrete tasks can be marked
/// `Plannable`, which lets downstream planning logic decide whether
/// post-plan readiness classification and refinement are likely to be useful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TaskClarity {
    /// The query is concrete enough to plan immediately.
    /// Examples: "ping google.com", "create a DNS record for example.com"
    Plannable,
    /// The query is too vague to plan — needs pre-planning elicitation.
    /// Examples: "help me with something", "I need to fix my server"
    #[default]
    NeedsElicitation,
}

/// Unified query analysis result combining all aspects
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedQueryAnalysis {
    pub original_query: String,

    // Complexity analysis
    pub complexity: ComplexityAnalysis,

    // Category analysis
    pub categories: CategoryAnalysis,

    // Dependency analysis
    pub dependencies: DependencyAnalysis,

    // Resource estimation
    pub resource_estimate: ResourceEstimate,

    // Parameter extraction
    pub extracted_entities: ExtractedEntities,

    // Intent classification (NEW)
    pub intent: QueryIntent,

    // Slot matching for elicitation (NEW)
    pub slot_match: Option<SlotMatchAnalysis>,

    // LLM call tracking
    #[serde(default)]
    pub llm_calls_used: u32,

    // Progressive planning (V2): task clarity classification
    #[serde(default)]
    pub task_clarity: TaskClarity,
}

impl UnifiedQueryAnalysis {
    /// Construct a minimal analysis for ask-loop pause early returns.
    ///
    /// No real analysis has occurred yet — just captures the query text
    /// with empty/default sub-analyses.
    pub fn minimal_for_pause(query: &str) -> Self {
        Self {
            original_query: query.to_string(),
            complexity: ComplexityAnalysis {
                score: 0.0,
                factors: Vec::new(),
                reasoning: String::new(),
            },
            categories: CategoryAnalysis {
                categories: Vec::new(),
                reasoning: String::new(),
            },
            dependencies: DependencyAnalysis {
                is_multi_step: false,
                dependencies: Vec::new(),
                workflow_steps: Vec::new(),
                reasoning: String::new(),
                required_capabilities: Vec::new(),
            },
            resource_estimate: ResourceEstimate {
                expected_tokens: 0,
                expected_duration_ms: 0,
                expected_iterations: 0,
            },
            extracted_entities: ExtractedEntities::default(),
            intent: QueryIntent::ContinueWorkflow,
            slot_match: None,
            llm_calls_used: 0,
            task_clarity: TaskClarity::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityAnalysis {
    pub score: f32,           // 0.0-1.0 complexity score
    pub factors: Vec<String>, // Contributing complexity factors
    #[serde(default)]
    pub reasoning: String, // LLM reasoning
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryAnalysis {
    pub categories: Vec<String>, // Identified tool categories
    pub reasoning: String,       // LLM reasoning
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequiredCapability {
    pub capability: String,
    pub description: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyAnalysis {
    pub is_multi_step: bool,
    pub dependencies: Vec<String>,   // List of identified dependencies
    pub workflow_steps: Vec<String>, // Ordered workflow steps if multi-step
    #[serde(default)]
    pub reasoning: String, // LLM reasoning

    // NEW: Required capabilities for completeness checking
    #[serde(default)]
    pub required_capabilities: Vec<RequiredCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceEstimate {
    pub expected_tokens: u32,
    pub expected_duration_ms: u32,
    pub expected_iterations: u32,
}

/// Unified analyzer that performs all query analysis in a single LLM call
pub struct UnifiedQueryAnalyzer {
    llm_service: Arc<dyn QueryAnalysisLLM>,
    tool_catalog: Arc<dyn ToolCatalog>,
    prompt_manager: Arc<PromptManager>,
    event_broadcaster:
        Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
}

impl UnifiedQueryAnalyzer {
    pub fn new(
        llm_service: Arc<dyn QueryAnalysisLLM>,
        tool_catalog: Arc<dyn ToolCatalog>,
        prompt_manager: Arc<PromptManager>,
    ) -> Self {
        Self {
            llm_service,
            tool_catalog,
            prompt_manager,
            event_broadcaster: None,
        }
    }

    pub fn with_event_broadcaster(
        mut self,
        broadcaster: Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
    ) -> Self {
        self.event_broadcaster = Some(broadcaster);
        self
    }

    /// Perform comprehensive query analysis in a single LLM call (context-free)
    pub async fn analyze_query(
        &self,
        query: &str,
        context: &ExecutionContext,
        available_tools: Option<&[ToolInfo]>,
        execution_id: Option<&str>,
        correlation_id: Option<&str>,
    ) -> Result<UnifiedQueryAnalysis> {
        info!(
            "[MAGICIAN-V2-QUERY] Starting query analysis for: {}",
            &query[..query.len().min(100)]
        );

        // Get available categories from runtime-visible tools.
        let available_categories = self.load_available_categories(context).await;
        debug!(
            "[MAGICIAN-V2-QUERY] Loaded {} available tool categories",
            available_categories.len()
        );

        // Perform unified analysis without conversation context
        let analysis = self
            .unified_llm_analysis(
                query,
                &available_categories,
                available_tools,
                None,
                context,
                execution_id,
                correlation_id,
            )
            .await?;

        let categories = analysis
            .categories
            .clone()
            .unwrap_or_else(|| CategoryAnalysis {
                categories: vec![],
                reasoning: "No categories provided".to_string(),
            });
        let complexity = analysis.complexity.clone();
        let dependencies = analysis.dependencies.clone();

        info!(
            "[MAGICIAN-V2-QUERY] ✅ LLM analysis completed - Complexity: {:.2}, Categories: {:?}",
            complexity.score, categories.categories
        );

        // Calculate resource estimate based on analysis
        let resource_estimate = self.calculate_resource_estimate(&analysis);

        Ok(UnifiedQueryAnalysis {
            original_query: query.to_string(),
            complexity,
            categories,
            dependencies,
            resource_estimate,
            extracted_entities: analysis.extracted_entities,
            intent: analysis.intent.unwrap_or(QueryIntent::NewTask), // Use LLM intent or default
            slot_match: analysis.slot_match,                         /* Will be None for
                                                                      * context-free analysis */
            llm_calls_used: 1, // One LLM call made in unified_llm_analysis()
            task_clarity: analysis.task_clarity,
        })
    }

    /// Perform comprehensive query analysis with conversation context (intent +
    /// slot matching)
    pub async fn analyze_query_with_context(
        &self,
        query: &str,
        context: &ExecutionContext,
        available_tools: Option<&[ToolInfo]>,
        conversation_context: ConversationContext,
        execution_id: Option<&str>,
        correlation_id: Option<&str>,
    ) -> Result<UnifiedQueryAnalysis> {
        // Get available categories from runtime-visible tools.
        let available_categories = self.load_available_categories(context).await;

        // Perform unified analysis WITH conversation context for intent detection and
        // slot matching
        let analysis = self
            .unified_llm_analysis(
                query,
                &available_categories,
                available_tools,
                Some(conversation_context),
                context,
                execution_id,
                correlation_id,
            )
            .await?;

        let categories = analysis
            .categories
            .clone()
            .unwrap_or_else(|| CategoryAnalysis {
                categories: vec![],
                reasoning: "No categories provided".to_string(),
            });
        let complexity = analysis.complexity.clone();
        let dependencies = analysis.dependencies.clone();

        // Calculate resource estimate based on analysis
        let resource_estimate = self.calculate_resource_estimate(&analysis);

        Ok(UnifiedQueryAnalysis {
            original_query: query.to_string(),
            complexity,
            categories,
            dependencies,
            resource_estimate,
            extracted_entities: analysis.extracted_entities,
            intent: analysis.intent.unwrap_or(QueryIntent::NewTask),
            slot_match: analysis.slot_match,
            llm_calls_used: 1, // One LLM call made in unified_llm_analysis()
            task_clarity: analysis.task_clarity,
        })
    }

    /// Load available categories from runtime-visible tools.
    async fn load_available_categories(&self, context: &ExecutionContext) -> Vec<String> {
        // Use catalog which applies runtime visibility filtering.
        self.tool_catalog.available_categories(context).await
    }

    /// Single unified LLM analysis call (with optional conversation context for
    /// intent detection)
    async fn unified_llm_analysis(
        &self,
        query: &str,
        available_categories: &[String],
        available_tools: Option<&[ToolInfo]>,
        conversation_context: Option<ConversationContext>,
        execution_context: &ExecutionContext,
        execution_id: Option<&str>,
        correlation_id: Option<&str>,
    ) -> Result<UnifiedAnalysisResult> {
        let prompt_name = names::UNIFIED_ANALYSIS;
        let prompt_version = versions::UNIFIED_ANALYSIS;

        // Create variables for prompt substitution
        let mut variables = HashMap::new();
        variables.insert("query".to_string(), query.to_string());
        let categories_context = if available_categories.is_empty() {
            "No predefined categories available. Generate appropriate categories based on the \
             query intent."
                .to_string()
        } else {
            format!(
                "Available categories in the system: {}",
                available_categories.join(", ")
            )
        };
        variables.insert("categories_context".to_string(), categories_context);

        let runtime_tools: &[ToolInfo] = available_tools.unwrap_or(&[]);
        if available_tools.is_none() {
            warn!(
                "[MAGICIAN-V2-QUERY] No explicit tool list provided to analyzer; using category-only context"
            );
        }
        variables.insert(
            "tools_description".to_string(),
            self.format_tools_for_prompt(runtime_tools),
        );

        // Add conversation context if provided
        if let Some(ref ctx) = conversation_context {
            variables.insert(
                "execution_status".to_string(),
                format!("{:?}", ctx.execution_status),
            );
            variables.insert(
                "pending_slots".to_string(),
                serde_json::to_string_pretty(&ctx.pending_slots)?,
            );
            variables.insert(
                "recent_turns".to_string(),
                self.format_recent_turns(&ctx.recent_turns),
            );
            variables.insert("has_conversation_context".to_string(), "true".to_string());
        } else {
            variables.insert("has_conversation_context".to_string(), "false".to_string());
        }

        // Get versioned prompt from storage
        info!(
            "[MAGICIAN-V2-QUERY] 📄 Loading prompt template: {} v{}",
            prompt_name, prompt_version
        );
        let prompt = self
            .prompt_manager
            .get_rendered_prompt(prompt_name, prompt_version, variables)
            .await?;
        info!("[MAGICIAN-V2-QUERY] ✅ Prompt template loaded successfully");

        let mut system_variables = HashMap::new();
        system_variables.insert(
            "identity_section".to_string(),
            render_prompt_identity_from_metadata(&execution_context.metadata, false),
        );
        let system_prompt = self
            .prompt_manager
            .get_rendered_prompt(
                names::UNIFIED_ANALYSIS_SYSTEM,
                versions::UNIFIED_ANALYSIS_SYSTEM,
                system_variables,
            )
            .await?;

        // Log comprehensive prompt parameters for debugging
        info!("[MAGICIAN-V2-QUERY] 🔍 LLM Prompt Parameters:");
        info!("[MAGICIAN-V2-QUERY]   Mode: Tool+category grounded");
        info!("[MAGICIAN-V2-QUERY]   Query: {}", query);
        info!(
            "[MAGICIAN-V2-QUERY]   Available Categories: {:?}",
            available_categories
        );
        info!(
            "[MAGICIAN-V2-QUERY]   Available Tools: {}",
            runtime_tools.len()
        );
        if let Some(ref ctx) = conversation_context {
            info!(
                "[MAGICIAN-V2-QUERY]   Execution Status: {:?}",
                ctx.execution_status
            );
            info!(
                "[MAGICIAN-V2-QUERY]   Pending Slots Count: {}",
                ctx.pending_slots.len()
            );
            info!(
                "[MAGICIAN-V2-QUERY]   Recent Turns Count: {}",
                ctx.recent_turns.len()
            );
        } else {
            info!("[MAGICIAN-V2-QUERY]   Conversation Context: None");
        }
        info!(
            "[MAGICIAN-V2-QUERY]   Prompt Length: {} chars",
            prompt.len()
        );

        // Log full prompt at debug level for detailed troubleshooting
        debug!("[MAGICIAN-V2-QUERY] 📝 Full LLM Prompt:\n{}", prompt);

        debug!(
            "[MAGICIAN-V2-QUERY] Sending query to LLM for analysis (prompt length: {} chars)",
            prompt.len()
        );

        // Emit LLM analysis started event
        if let (Some(tid), Some(cid), Some(ref broadcaster)) =
            (execution_id, correlation_id, &self.event_broadcaster)
        {
            broadcaster.llm_analysis_started(
                tid,
                cid,
                self.llm_service.provider_name(),
                "query_analysis".to_string(),
                prompt.len(),
            );
        }

        // Call LLM and measure time
        let start = std::time::Instant::now();
        let llm_response = match self
            .llm_service
            .generate_analysis_with_system_scoped(
                magicllm::LlmScope::new(
                    execution_context.principal.clone(),
                    execution_context.workspace.clone(),
                ),
                Some(&system_prompt),
                &prompt,
            )
            .await
        {
            Ok(resp) => {
                let duration = start.elapsed();

                // Emit LLM analysis completed event
                if let (Some(tid), Some(cid), Some(ref broadcaster)) =
                    (execution_id, correlation_id, &self.event_broadcaster)
                {
                    broadcaster.llm_analysis_completed(
                        tid,
                        cid,
                        self.llm_service.provider_name(),
                        "query_analysis".to_string(),
                        resp.content.len(),
                        duration.as_millis() as u64,
                    );
                }

                if let Some(ref usage) = resp.usage {
                    info!(
                        "[MAGICIAN-V2-QUERY] ✅ LLM responded in {:.2}s (response: {} chars, tokens: {})",
                        duration.as_secs_f64(),
                        resp.content.len(),
                        usage.total_tokens
                    );
                } else {
                    info!(
                        "[MAGICIAN-V2-QUERY] ✅ LLM responded in {:.2}s (response length: {} chars)",
                        duration.as_secs_f64(),
                        resp.content.len()
                    );
                }
                resp
            },
            Err(e) => {
                let duration = start.elapsed();

                // Emit LLM analysis failed event
                if let (Some(tid), Some(cid), Some(ref broadcaster)) =
                    (execution_id, correlation_id, &self.event_broadcaster)
                {
                    let error_type = classify_llm_failure(&e).to_string();

                    broadcaster.llm_analysis_failed(
                        tid,
                        cid,
                        self.llm_service.provider_name(),
                        "query_analysis".to_string(),
                        error_type,
                        e.to_string(),
                    );
                }

                error!(
                    "[MAGICIAN-V2-QUERY] ❌ LLM call failed after {:.2}s: {}",
                    duration.as_secs_f64(),
                    e
                );
                return Err(e);
            },
        };

        let parsed = self.parse_unified_response(&llm_response.content);
        if let Some(broadcaster) = self.event_broadcaster.as_ref() {
            let telemetry = OperationLlmTelemetryContext::new(
                Arc::clone(broadcaster),
                execution_context.principal.clone(),
                execution_context.workspace.clone(),
                "query_analysis",
            );
            let attribution = OperationLlmCallAttribution {
                execution_id: execution_id.map(str::to_string),
                task_id: execution_context.metadata.get("task_id").cloned(),
                agent_id: execution_context.metadata.get("agent_id").cloned(),
                chat_session_id: execution_context.metadata.get("chat_session_id").cloned(),
                ..OperationLlmCallAttribution::default()
            };
            let latency_ms = start.elapsed().as_millis() as u64;
            match parsed.as_ref() {
                Ok(_) => telemetry.emit_validated_success(
                    "query_analysis",
                    &llm_response,
                    latency_ms,
                    attribution,
                    "query_analysis_json",
                ),
                Err(error) => telemetry.emit_validation_failure(
                    "query_analysis",
                    &llm_response,
                    latency_ms,
                    attribution,
                    "query_analysis_json",
                    &error.to_string(),
                ),
            }
        }

        // Parse response (fail fast if invalid JSON)
        match parsed {
            Ok(result) => {
                debug!("[MAGICIAN-V2-QUERY] Successfully parsed LLM response");
                Ok(result)
            },
            Err(e) => {
                error!("[MAGICIAN-V2-QUERY] ❌ Failed to parse LLM response: {}", e);
                Err(e)
            },
        }
    }

    /// Format recent turns for context
    fn format_recent_turns(&self, turns: &[V2Turn]) -> String {
        if turns.is_empty() {
            return "No conversation history".to_string();
        }

        let mut formatted = String::new();
        for (idx, turn) in turns.iter().rev().take(3).enumerate() {
            let direction = match turn.direction {
                TurnDirection::Inbound => "user",
                TurnDirection::Outbound => "assistant",
            };
            formatted.push_str(&format!(
                "Turn {}: [{}] {}\n",
                idx + 1,
                direction,
                turn.text
            ));
        }
        formatted
    }

    /// Format visible runtime tools for prompt grounding.
    fn format_tools_for_prompt(&self, tools: &[ToolInfo]) -> String {
        const MAX_TOOLS_FOR_PROMPT: usize = 80;
        const MAX_REQUIRED_PARAMS: usize = 6;
        const MAX_DESC_CHARS: usize = 160;

        if tools.is_empty() {
            return "No runtime tools are currently available.".to_string();
        }

        let shown = tools.len().min(MAX_TOOLS_FOR_PROMPT);
        let mut lines = Vec::with_capacity(shown + 2);
        lines.push(format!(
            "Visible runtime tools: {} (showing {})",
            tools.len(),
            shown
        ));

        for tool in tools.iter().take(MAX_TOOLS_FOR_PROMPT) {
            let category = tool
                .composition_category
                .as_deref()
                .unwrap_or(&tool.category);
            let required_params = tool
                .parameters
                .iter()
                .filter(|param| param.required)
                .take(MAX_REQUIRED_PARAMS)
                .map(|param| param.name.as_str())
                .collect::<Vec<_>>();
            let required_params = if required_params.is_empty() {
                "none".to_string()
            } else {
                required_params.join(", ")
            };
            let raw_description = tool
                .enhanced_description
                .as_deref()
                .unwrap_or(&tool.description)
                .replace('\n', " ");
            let summary = raw_description
                .chars()
                .take(MAX_DESC_CHARS)
                .collect::<String>();

            lines.push(format!(
                "- {} | category={} | required_params={} | {}",
                tool.name, category, required_params, summary
            ));
        }

        if tools.len() > MAX_TOOLS_FOR_PROMPT {
            lines.push(format!(
                "... {} additional tools omitted for brevity",
                tools.len() - MAX_TOOLS_FOR_PROMPT
            ));
        }

        lines.join("\n")
    }

    /// Parse unified LLM response with robust JSON extraction
    /// Tries multiple extraction strategies to handle different LLM response
    /// formats
    fn parse_unified_response(&self, response: &str) -> Result<UnifiedAnalysisResult> {
        // Strategy 1: Try direct JSON parse (for well-behaved LLMs)
        if let Ok(result) = serde_json::from_str::<UnifiedAnalysisResult>(response) {
            debug!("[MAGICIAN-V2-QUERY] ✅ Parsed JSON directly (no extraction needed)");
            debug!(
                "[MAGICIAN-V2-QUERY] [LLM-RESPONSE-DEBUG] Raw LLM response: {}",
                response
            );
            Self::log_parsed_result(&result);
            return Ok(result);
        }

        // Strategy 2: Extract from markdown code blocks (```json ... ```)
        if let Some(extracted) = Self::extract_json_from_code_block(response) {
            debug!("[MAGICIAN-V2-QUERY] 🔧 Extracted JSON from markdown code block");
            let result =
                serde_json::from_str::<UnifiedAnalysisResult>(&extracted).map_err(|e| {
                    anyhow::anyhow!(
                        "LLM returned JSON in code block but it's invalid: {}. Extracted JSON \
                         preview: {}",
                        e,
                        &extracted[..extracted.len().min(500)]
                    )
                })?;
            Self::log_parsed_result(&result);
            return Ok(result);
        }

        // Strategy 3: Extract between first { and last } (for mixed content)
        if let Some(extracted) = Self::extract_json_from_braces(response) {
            debug!("[MAGICIAN-V2-QUERY] 🔧 Extracted JSON from brace boundaries");
            let result =
                serde_json::from_str::<UnifiedAnalysisResult>(&extracted).map_err(|e| {
                    anyhow::anyhow!(
                        "LLM response contains JSON-like content but it's invalid: {}. Extracted \
                         JSON preview: {}",
                        e,
                        &extracted[..extracted.len().min(2000)]
                    )
                })?;
            Self::log_parsed_result(&result);
            return Ok(result);
        }

        // All strategies failed - return clear error with preview
        Err(anyhow::anyhow!(
            "LLM returned invalid JSON (all extraction strategies failed). Response preview: {}",
            &response[..response.len().min(500)]
        ))
    }

    /// Log parsed LLM analysis result for debugging
    fn log_parsed_result(result: &UnifiedAnalysisResult) {
        info!("[MAGICIAN-V2-QUERY] 📊 Parsed LLM Analysis Result:");

        // Log intent if present
        if let Some(ref intent) = result.intent {
            info!("[MAGICIAN-V2-QUERY]   Intent: {}", intent);
        }

        // Log complexity analysis
        info!(
            "[MAGICIAN-V2-QUERY]   Complexity: score={:.2}, factors={:?}",
            result.complexity.score, result.complexity.factors
        );

        // Log categories
        if let Some(ref categories) = result.categories {
            info!(
                "[MAGICIAN-V2-QUERY]   Categories: {:?} (reasoning: {})",
                categories.categories, categories.reasoning
            );
        } else {
            info!("[MAGICIAN-V2-QUERY]   Categories: None");
        }

        // Log dependencies
        info!(
            "[MAGICIAN-V2-QUERY]   Multi-step: {}, dependencies={:?}",
            result.dependencies.is_multi_step, result.dependencies.dependencies
        );

        // Log extracted entities
        if !result.extracted_entities.entities.is_empty() {
            info!("[MAGICIAN-V2-QUERY]   Extracted Entities:");
            for (key, value) in &result.extracted_entities.entities {
                info!("[MAGICIAN-V2-QUERY]     {}: {}", key, value);
            }
        }

        // Log typed entities
        debug!(
            "[MAGICIAN-V2-QUERY] [LLM-ENTITIES-DEBUG] entities: {:?}",
            result.extracted_entities.entities
        );
        debug!(
            "[MAGICIAN-V2-QUERY] [LLM-ENTITIES-DEBUG] typed_entities: {:?}",
            result.extracted_entities.typed_entities
        );

        if !result.extracted_entities.typed_entities.is_empty() {
            info!("[MAGICIAN-V2-QUERY]   Typed Entities:");
            for (entity_type, values) in &result.extracted_entities.typed_entities {
                info!("[MAGICIAN-V2-QUERY]     {}: {:?}", entity_type, values);
            }
        } else {
            info!("[MAGICIAN-V2-QUERY]   Typed Entities: (none extracted)");
        }

        // Log slot match if present
        if let Some(ref slot_match) = result.slot_match {
            info!(
                "[MAGICIAN-V2-QUERY]   Slot Match: slot_id={}, value={:?}, confidence={:.2}",
                slot_match.slot_id, slot_match.extracted_value, slot_match.match_confidence
            );
        }

        info!(
            "[MAGICIAN-V2-QUERY]   Extraction Confidence: {:.2}",
            result.extracted_entities.extraction_confidence
        );
    }

    /// Extract JSON from markdown code blocks: ```json ... ```
    fn extract_json_from_code_block(text: &str) -> Option<String> {
        // Look for ```json\n{...}\n```
        if let Some(start_marker) = text.find("```json") {
            let content_start = start_marker + 7; // Skip past ```json
            if let Some(end_marker) = text[content_start..].find("```") {
                let json_content = &text[content_start..content_start + end_marker];
                return Some(json_content.trim().to_string());
            }
        }

        // Also try plain ``` without json marker
        if let Some(start_marker) = text.find("```") {
            let content_start = start_marker + 3;
            // Skip optional language identifier (e.g., ```json)
            let content_after_marker = &text[content_start..];
            let actual_start = if let Some(newline) = content_after_marker.find('\n') {
                content_start + newline + 1
            } else {
                content_start
            };

            if let Some(end_marker) = text[actual_start..].find("```") {
                let json_content = &text[actual_start..actual_start + end_marker];
                let trimmed = json_content.trim();
                // Only return if it looks like JSON (starts with {)
                if trimmed.starts_with('{') {
                    return Some(trimmed.to_string());
                }
            }
        }

        None
    }

    /// Extract JSON from first { to last }
    fn extract_json_from_braces(text: &str) -> Option<String> {
        if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                if end > start {
                    return Some(text[start..=end].to_string());
                }
            }
        }
        None
    }

    /// Calculate resource requirements based on analysis
    fn calculate_resource_estimate(&self, analysis: &UnifiedAnalysisResult) -> ResourceEstimate {
        let base_tokens = 100;
        let base_duration_ms = 500;
        let base_iterations = 1;

        // Scale based on complexity
        let complexity_multiplier = 1.0 + (analysis.complexity.score * 3.0);

        // Additional scaling for multi-step workflows
        let multi_step_multiplier = if analysis.dependencies.is_multi_step {
            2.0
        } else {
            1.0
        };

        // Category complexity factor (handle optional categories)
        let category_count = analysis
            .categories
            .as_ref()
            .map(|c| c.categories.len())
            .unwrap_or(0);
        let category_multiplier = 1.0 + (category_count as f32 * 0.2);

        let total_multiplier = complexity_multiplier * multi_step_multiplier * category_multiplier;

        ResourceEstimate {
            expected_tokens: (base_tokens as f32 * total_multiplier) as u32,
            expected_duration_ms: (base_duration_ms as f32 * total_multiplier) as u32,
            expected_iterations: std::cmp::max(
                1,
                (base_iterations as f32 * complexity_multiplier) as u32,
            ),
        }
    }
}

fn classify_llm_failure(error: &anyhow::Error) -> &'static str {
    if let Some(llm_error) = error.downcast_ref::<LLMError>() {
        return match llm_error.root_cause() {
            LLMError::Routed { .. } => unreachable!("root_cause removes routed wrappers"),
            LLMError::Context(_) => "context_window_exceeded",
            LLMError::Timeout => "timeout",
            LLMError::Transport(_) => "network_error",
            LLMError::ProviderStatus { status, .. } => {
                if *status == 429 {
                    "rate_limited"
                } else if (500..=599).contains(status) {
                    "server_error"
                } else {
                    "provider_error"
                }
            },
            LLMError::Configuration(_) => "configuration_error",
            LLMError::Validation(_) => "validation_error",
            LLMError::UnsupportedCapability(_) => "unsupported_capability",
            LLMError::Serialization(_) => "serialization_error",
            LLMError::Provider { .. } => "provider_error",
            LLMError::RateLimited { .. } => "rate_limited",
            LLMError::Cancelled { .. } => "cancelled",
            LLMError::AllRetriesExhausted { .. } => "retries_exhausted",
            LLMError::ProviderUnavailable => "provider_unavailable",
            LLMError::WorkerWatchdog { .. } => "watchdog",
            LLMError::DeadlineExceeded => "deadline_exceeded",
            LLMError::QueueFull { .. } => "queue_full",
            LLMError::RequestTooLarge { .. } => "request_too_large",
            LLMError::QueueBytesFull { .. } => "queue_bytes_full",
            LLMError::Other(_) => "provider_error",
        };
    }

    if let Some(reqwest_error) = error.downcast_ref::<ReqwestError>() {
        if reqwest_error.is_timeout() {
            return "timeout";
        }
        if reqwest_error.is_connect() {
            return "network_error";
        }
        return "provider_error";
    }

    if error.chain().any(|cause| cause.is::<Elapsed>()) {
        return "timeout";
    }

    "unknown_error"
}

/// Internal result structure for parsing
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UnifiedAnalysisResult {
    complexity: ComplexityAnalysis,
    #[serde(default)]
    categories: Option<CategoryAnalysis>,
    dependencies: DependencyAnalysis,
    #[serde(default)]
    extracted_entities: ExtractedEntities,

    // NEW: Intent classification (only present when conversation context provided)
    #[serde(default)]
    intent: Option<QueryIntent>,

    // NEW: Slot matching (only present when AnswerElicitation intent detected)
    #[serde(default)]
    slot_match: Option<SlotMatchAnalysis>,

    // Progressive planning (V2): task clarity classification
    #[serde(default)]
    task_clarity: TaskClarity,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use runtime_core::{ExecutionContext, ToolDiscovery};

    use super::*;
    use crate::magician_v2::{
        query_analysis::operation_llm_router::SimplifiedLLMResponse, tooling::ToolDiscoveryAdapter,
    };

    /// Mock LLM service for testing
    #[derive(Clone)]
    pub struct MockQueryAnalysisLLM {
        /// Predefined responses for different query patterns
        responses: Arc<Mutex<HashMap<String, String>>>,
        /// Call count for testing
        call_count: Arc<Mutex<u32>>,
    }

    impl MockQueryAnalysisLLM {
        pub fn new() -> Self {
            let mut responses = HashMap::new();

            // Simple query response
            responses.insert(
                "create a file".to_string(),
                r#"{
                    "complexity": {
                        "score": 0.2,
                        "factors": ["single_operation", "filesystem"],
                        "reasoning": "Simple file creation operation"
                    },
                    "categories": {
                        "categories": ["filesystem"],
                        "reasoning": "File operation requires filesystem tools"
                    },
                    "dependencies": {
                        "is_multi_step": false,
                        "dependencies": [],
                        "workflow_steps": ["create_file"],
                        "reasoning": "Single atomic operation"
                    },
                    "extracted_entities": {
                        "entities": {},
                        "typed_entities": {},
                        "extraction_confidence": 0.5,
                        "extraction_reasoning": "No specific parameters mentioned in query"
                    }
                }"#
                .to_string(),
            );

            // Complex query response
            responses.insert(
                "deploy my app to AWS with CI/CD".to_string(),
                r#"{
                    "complexity": {
                        "score": 0.9,
                        "factors": ["multiple_services", "cloud_deployment", "automation"],
                        "reasoning": "Complex deployment with multiple integrated services"
                    },
                    "categories": {
                        "categories": ["deployment", "cloud", "automation"],
                        "reasoning": "Requires deployment tools, cloud services, and CI/CD automation"
                    },
                    "dependencies": {
                        "is_multi_step": true,
                        "dependencies": ["build_step", "test_step", "deployment_step"],
                        "workflow_steps": ["test_code", "build_artifact", "deploy_to_staging", "run_tests", "deploy_to_production"],
                        "reasoning": "Multi-step workflow with dependencies between stages"
                    },
                    "extracted_entities": {
                        "entities": {
                            "platform": "AWS"
                        },
                        "typed_entities": {
                            "platform": ["AWS"]
                        },
                        "extraction_confidence": 0.9,
                        "extraction_reasoning": "Extracted platform 'AWS' from explicit mention in query"
                    }
                }"#.to_string(),
            );

            // Multi-step workflow response
            responses.insert(
                "test, build, and deploy my application".to_string(),
                r#"{
                    "complexity": {
                        "score": 0.7,
                        "factors": ["sequential_operations", "multiple_tools", "validation_steps"],
                        "reasoning": "Sequential workflow with validation at each step"
                    },
                    "categories": {
                        "categories": ["testing", "build", "deployment"],
                        "reasoning": "Covers three main development lifecycle categories"
                    },
                    "dependencies": {
                        "is_multi_step": true,
                        "dependencies": ["test_before_build", "build_before_deploy"],
                        "workflow_steps": ["run_tests", "build_application", "deploy_application"],
                        "reasoning": "Clear sequential dependencies between operations"
                    },
                    "extracted_entities": {
                        "entities": {},
                        "typed_entities": {},
                        "extraction_confidence": 0.4,
                        "extraction_reasoning": "No specific parameters mentioned, only general workflow actions"
                    }
                }"#.to_string(),
            );

            // Invalid JSON response for fallback testing
            responses.insert(
                "invalid_response".to_string(),
                "This is not valid JSON".to_string(),
            );

            Self {
                responses: Arc::new(Mutex::new(responses)),
                call_count: Arc::new(Mutex::new(0)),
            }
        }

        pub fn add_response(&self, query: &str, response: &str) {
            let mut responses = self.responses.lock().unwrap();
            responses.insert(query.to_string(), response.to_string());
        }

        pub fn get_call_count(&self) -> u32 {
            *self.call_count.lock().unwrap()
        }
    }

    #[async_trait]
    impl QueryAnalysisLLM for MockQueryAnalysisLLM {
        async fn generate_analysis(&self, prompt: &str) -> Result<SimplifiedLLMResponse> {
            let mut count = self.call_count.lock().unwrap();
            *count += 1;

            let responses = self.responses.lock().unwrap();

            // Extract query from prompt - look for the query pattern
            let lines: Vec<&str> = prompt.lines().collect();
            let query_line = lines
                .iter()
                .find(|line| line.to_lowercase().contains("query:"))
                .unwrap_or(&"");

            let query_key = query_line
                .replace("Query:", "")
                .replace("\"", "")
                .trim()
                .to_lowercase();

            let response_text = responses.get(&query_key).cloned().unwrap_or_else(|| {
                r#"{
                    "complexity": {
                        "score": 0.5,
                        "factors": ["unknown"],
                        "reasoning": "Default response for unrecognized query"
                    },
                    "categories": {
                        "categories": [],
                        "reasoning": "No categories detected - will use fallback"
                    },
                    "dependencies": {
                        "is_multi_step": false,
                        "dependencies": [],
                        "workflow_steps": ["default_step"],
                        "reasoning": "Default single step"
                    }
                }"#
                .to_string()
            });

            Ok(SimplifiedLLMResponse::content_only(response_text))
        }
    }

    struct SystemPromptGuardLLM {
        seen_system_prompts: Arc<Mutex<Vec<String>>>,
    }

    impl SystemPromptGuardLLM {
        fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    seen_system_prompts: Arc::clone(&seen),
                },
                seen,
            )
        }
    }

    #[async_trait]
    impl QueryAnalysisLLM for SystemPromptGuardLLM {
        async fn generate_analysis(&self, _prompt: &str) -> Result<SimplifiedLLMResponse> {
            Err(anyhow::anyhow!(
                "generate_analysis() should not be used when system prompt path is wired"
            ))
        }

        async fn generate_analysis_with_system(
            &self,
            system_prompt: Option<&str>,
            _prompt: &str,
        ) -> Result<SimplifiedLLMResponse> {
            self.seen_system_prompts.lock().unwrap().push(
                system_prompt
                    .map(str::to_string)
                    .unwrap_or_else(|| "<none>".to_string()),
            );

            Ok(SimplifiedLLMResponse::content_only(
                r#"{
                    "complexity": {
                        "score": 0.4,
                        "factors": ["single_step"],
                        "reasoning": "simple"
                    },
                    "categories": {
                        "categories": ["filesystem"],
                        "reasoning": "single category"
                    },
                    "dependencies": {
                        "is_multi_step": false,
                        "dependencies": [],
                        "workflow_steps": ["step1"],
                        "reasoning": "single step",
                        "required_capabilities": []
                    },
                    "extracted_entities": {
                        "entities": {},
                        "typed_entities": {},
                        "extraction_confidence": 0.5,
                        "extraction_reasoning": "none"
                    },
                    "intent": "new_task",
                    "slot_match": null
                }"#,
            ))
        }
    }

    /// Mock ToolDiscovery for testing
    struct MockToolDiscovery {
        categories: Vec<String>,
    }

    impl MockToolDiscovery {
        fn new() -> Self {
            Self { categories: vec![] }
        }

        #[allow(dead_code)]
        fn with_categories(categories: Vec<String>) -> Self {
            Self { categories }
        }
    }

    #[async_trait::async_trait]
    impl ToolDiscovery for MockToolDiscovery {
        async fn find_best_match_with_context(
            &self,
            _task_description: &str,
            _context: &ExecutionContext,
        ) -> crate::ToolMatchResult {
            crate::ToolMatchResult {
                primary_match: None,
                match_confidence: 0.0,
                missing_capabilities: vec![],
                parameter_coverage: 0.0,
                executable: false,
            }
        }

        async fn find_multiple_matches_with_context(
            &self,
            _task_description: &str,
            _context: &ExecutionContext,
        ) -> crate::MultipleToolMatchResult {
            crate::MultipleToolMatchResult {
                matches: vec![],
                match_strategies: vec![],
                confidence_spread: 0.0,
                recommended_approach: None,
                any_executable: false,
                aggregate_missing_capabilities: vec![],
            }
        }

        async fn is_tool_available(&self, _tool_name: &str, _context: &ExecutionContext) -> bool {
            false
        }

        async fn get_tool_metadata(
            &self,
            _tool_name: &str,
            _context: &ExecutionContext,
        ) -> Option<std::collections::HashMap<String, serde_json::Value>> {
            None
        }

        async fn get_available_tools(&self, _context: &ExecutionContext) -> Vec<String> {
            vec![]
        }

        async fn get_available_categories(&self, _context: &ExecutionContext) -> Vec<String> {
            self.categories.clone()
        }
    }

    /// Create a mock tool discovery for testing
    fn create_mock_tool_discovery() -> Arc<dyn ToolDiscovery> {
        Arc::new(MockToolDiscovery::new())
    }

    /// Create a mock prompt manager for testing
    async fn create_mock_prompt_manager() -> Result<Arc<PromptManager>> {
        use crate::magician_v2::prompts::{json_storage::JsonStorageConfig, JsonPromptStorage};

        let data_dir = crate::magician_v2::prompts::json_storage::default_prompt_dir();
        let config = JsonStorageConfig {
            storage_dir: data_dir,
            enable_cache: false,
            max_cache_entries: 1,
        };

        let storage = JsonPromptStorage::new(config)?;
        // JsonPromptStorage doesn't have initialize method, just create it

        Ok(Arc::new(PromptManager::new(Arc::new(storage))))
    }

    #[tokio::test]
    async fn test_analyze_simple_query() {
        let mock_llm = Arc::new(MockQueryAnalysisLLM::new());
        let tool_catalog: Arc<dyn ToolCatalog> =
            Arc::new(ToolDiscoveryAdapter::new(create_mock_tool_discovery()));
        let prompt_manager = create_mock_prompt_manager().await.unwrap();

        let analyzer = UnifiedQueryAnalyzer::new(mock_llm.clone(), tool_catalog, prompt_manager);

        let context = ExecutionContext::default();
        let result = analyzer
            .analyze_query("create a file", &context, None, None, None)
            .await;

        // Should succeed because prompt files now exist in the actual data directory
        assert!(result.is_ok());
        assert!(mock_llm.get_call_count() > 0); // Should call LLM with the
                                                // loaded prompt
    }

    #[tokio::test]
    async fn test_analyze_query_uses_system_prompt_call_path() {
        let (mock_llm, seen_system_prompts) = SystemPromptGuardLLM::new();
        let tool_catalog: Arc<dyn ToolCatalog> =
            Arc::new(ToolDiscoveryAdapter::new(create_mock_tool_discovery()));
        let prompt_manager = create_mock_prompt_manager().await.unwrap();
        let analyzer = UnifiedQueryAnalyzer::new(Arc::new(mock_llm), tool_catalog, prompt_manager);

        let identity = crate::magician_v2::execution::PromptIdentityContext {
            agent_kind: Some(crate::magician_v2::execution::PromptAgentKind::User),
            base_persona: Some("Precise".to_string()),
            source_agent_id: Some("agent-unified-test".to_string()),
            source_agent_name: Some("Analyst".to_string()),
            source_agent_aliases: Vec::new(),
            source_agent_persona: None,
            autonomous_controls: None,
        };
        let mut context = ExecutionContext::default();
        context.metadata.insert(
            "agent:prompt_identity".to_string(),
            serde_json::to_string(&identity).expect("serialize prompt identity"),
        );

        let result = analyzer
            .analyze_query("create a file", &context, None, None, None)
            .await
            .expect("analysis should succeed through system prompt path");
        assert_eq!(result.complexity.score, 0.4);

        let prompts = seen_system_prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(
            prompts[0].contains("AGENT IDENTITY CONTEXT"),
            "system prompt should include identity section"
        );
        assert!(
            prompts[0].contains("Source agent id: agent-unified-test"),
            "system prompt should include source agent id"
        );
    }

    #[tokio::test]
    async fn test_load_available_categories() {
        let mock_llm = Arc::new(MockQueryAnalysisLLM::new());
        let tool_catalog: Arc<dyn ToolCatalog> =
            Arc::new(ToolDiscoveryAdapter::new(create_mock_tool_discovery()));
        let prompt_manager = create_mock_prompt_manager().await.unwrap();

        let analyzer = UnifiedQueryAnalyzer::new(mock_llm, tool_catalog, prompt_manager);

        let context = ExecutionContext::default();
        let categories = analyzer.load_available_categories(&context).await;

        // With empty mock, should return empty categories (this is correct behavior)
        // Tests that the function works without crashing even with no tools
        assert!(categories.is_empty());
    }

    #[tokio::test]
    async fn test_parse_unified_response_valid_json() {
        let mock_llm = Arc::new(MockQueryAnalysisLLM::new());
        let tool_catalog: Arc<dyn ToolCatalog> =
            Arc::new(ToolDiscoveryAdapter::new(create_mock_tool_discovery()));
        let prompt_manager = create_mock_prompt_manager().await.unwrap();

        let analyzer = UnifiedQueryAnalyzer::new(mock_llm, tool_catalog, prompt_manager);

        let valid_response = r#"{
            "complexity": {
                "score": 0.3,
                "factors": ["test_factor"],
                "reasoning": "Test reasoning"
            },
            "categories": {
                "categories": ["test_category"],
                "reasoning": "Test category reasoning"
            },
            "dependencies": {
                "is_multi_step": true,
                "dependencies": ["dep1"],
                "workflow_steps": ["step1", "step2"],
                "reasoning": "Test dependency reasoning"
            }
        }"#;

        let result = analyzer.parse_unified_response(valid_response).unwrap();

        assert_eq!(result.complexity.score, 0.3);
        assert_eq!(result.complexity.factors, vec!["test_factor"]);
        assert_eq!(
            result.categories.as_ref().unwrap().categories,
            vec!["test_category"]
        );
        assert!(result.dependencies.is_multi_step);
        assert_eq!(result.dependencies.workflow_steps, vec!["step1", "step2"]);
    }

    #[tokio::test]
    async fn test_parse_unified_response_fails_on_invalid_json() {
        let mock_llm = Arc::new(MockQueryAnalysisLLM::new());
        let tool_catalog: Arc<dyn ToolCatalog> =
            Arc::new(ToolDiscoveryAdapter::new(create_mock_tool_discovery()));
        let prompt_manager = create_mock_prompt_manager().await.unwrap();

        let analyzer = UnifiedQueryAnalyzer::new(mock_llm, tool_catalog, prompt_manager);

        let invalid_response = "Invalid JSON response";

        // Should return error, NOT fallback
        let result = analyzer.parse_unified_response(invalid_response);
        assert!(
            result.is_err(),
            "Expected error for invalid JSON, got success"
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("LLM returned invalid JSON"));
    }

    #[tokio::test]
    async fn test_calculate_resource_estimate() {
        let mock_llm = Arc::new(MockQueryAnalysisLLM::new());
        let tool_catalog: Arc<dyn ToolCatalog> =
            Arc::new(ToolDiscoveryAdapter::new(create_mock_tool_discovery()));
        let prompt_manager = create_mock_prompt_manager().await.unwrap();

        let analyzer = UnifiedQueryAnalyzer::new(mock_llm, tool_catalog, prompt_manager);

        let simple_analysis = UnifiedAnalysisResult {
            complexity: ComplexityAnalysis {
                score: 0.2,
                factors: vec!["simple".to_string()],
                reasoning: "Simple operation".to_string(),
            },
            categories: Some(CategoryAnalysis {
                categories: vec!["filesystem".to_string()],
                reasoning: "File operation".to_string(),
            }),
            dependencies: DependencyAnalysis {
                is_multi_step: false,
                dependencies: vec![],
                workflow_steps: vec!["create_file".to_string()],
                reasoning: "Single step".to_string(),
                required_capabilities: vec![],
            },
            extracted_entities: ExtractedEntities::default(),
            intent: None,
            slot_match: None,
            task_clarity: TaskClarity::default(),
        };

        let estimate = analyzer.calculate_resource_estimate(&simple_analysis);

        // Should calculate based on low complexity, single category, single step
        assert!(estimate.expected_tokens >= 100);
        assert!(estimate.expected_duration_ms >= 500);
        assert_eq!(estimate.expected_iterations, 1);

        // Test complex multi-step analysis
        let complex_analysis = UnifiedAnalysisResult {
            complexity: ComplexityAnalysis {
                score: 0.9,
                factors: vec!["complex".to_string(), "multi_service".to_string()],
                reasoning: "Complex operation".to_string(),
            },
            categories: Some(CategoryAnalysis {
                categories: vec![
                    "deployment".to_string(),
                    "cloud".to_string(),
                    "automation".to_string(),
                ],
                reasoning: "Multi-category operation".to_string(),
            }),
            dependencies: DependencyAnalysis {
                is_multi_step: true,
                dependencies: vec!["dep1".to_string(), "dep2".to_string()],
                workflow_steps: vec![
                    "step1".to_string(),
                    "step2".to_string(),
                    "step3".to_string(),
                ],
                reasoning: "Multi-step workflow".to_string(),
                required_capabilities: vec![],
            },
            extracted_entities: ExtractedEntities::default(),
            intent: None,
            slot_match: None,
            task_clarity: TaskClarity::default(),
        };

        let complex_estimate = analyzer.calculate_resource_estimate(&complex_analysis);

        // Should have higher estimates for complex operations
        assert!(complex_estimate.expected_tokens > estimate.expected_tokens);
        assert!(complex_estimate.expected_duration_ms > estimate.expected_duration_ms);
        assert!(complex_estimate.expected_iterations >= estimate.expected_iterations);
    }

    #[test]
    fn test_mock_llm_responses() {
        let mock_llm = MockQueryAnalysisLLM::new();

        // Test that we can add and retrieve custom responses
        mock_llm.add_response("test query", "test response");

        let responses = mock_llm.responses.lock().unwrap();
        assert_eq!(
            responses.get("test query"),
            Some(&"test response".to_string())
        );

        // Test that default responses exist
        assert!(responses.contains_key("create a file"));
        assert!(responses.contains_key("deploy my app to AWS with CI/CD"));
    }
}
