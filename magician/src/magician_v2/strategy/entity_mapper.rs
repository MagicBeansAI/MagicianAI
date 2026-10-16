//! Entity-to-Parameter Mapping Service
//!
//! This module provides intelligent mapping of extracted entities to tool
//! parameters using LLM-based semantic understanding and type conversion.
//!
//! # Purpose
//! The query analyzer extracts entities (targets, ports, files, etc.) from user
//! queries. This module maps those entities to the specific parameter names
//! required by tools, handling semantic differences (e.g., "target" →
//! "hostname") and type conversions (e.g., string "443" → integer 443).

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context as AnyhowContext, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::magician_v2::{
    analytics::operation_llm_telemetry::{
        OperationLlmCallAttribution, OperationLlmTelemetryContext,
    },
    prompts::{constants, PromptManager},
    query_analysis::operation_llm_router::QueryAnalysisLLM,
};

/// Entity mapper service that uses LLM for intelligent parameter mapping
pub struct EntityMapper {
    llm_service: Arc<dyn QueryAnalysisLLM>,
    prompt_manager: Arc<PromptManager>,
    llm_telemetry: Option<(OperationLlmTelemetryContext, OperationLlmCallAttribution)>,
}

impl EntityMapper {
    /// Create new entity mapper with LLM service and prompt manager
    pub fn new(llm_service: Arc<dyn QueryAnalysisLLM>, prompt_manager: Arc<PromptManager>) -> Self {
        Self {
            llm_service,
            prompt_manager,
            llm_telemetry: None,
        }
    }

    pub fn with_llm_telemetry(
        mut self,
        context: OperationLlmTelemetryContext,
        attribution: OperationLlmCallAttribution,
    ) -> Self {
        self.llm_telemetry = Some((context, attribution));
        self
    }

    /// Map extracted entities to tool parameters using semantic understanding
    ///
    /// # Arguments
    /// * `tool_schema` - JSON schema of the tool's parameters
    /// * `extracted_entities` - Entities extracted from the query
    /// * `query_text` - Original query for context
    ///
    /// # Returns
    /// * `ParameterMappingResult` with mapped parameters and metadata
    pub async fn map_entities_to_parameters(
        &self,
        tool_schema: &serde_json::Value,
        extracted_entities: &HashMap<String, String>,
        query_text: &str,
    ) -> Result<ParameterMappingResult> {
        debug!(
            "[MAGICIAN-V2-STRATEGY] Mapping {} entities to tool parameters",
            extracted_entities.len()
        );

        // Quick path: if no entities, return empty mapping
        if extracted_entities.is_empty() {
            return Ok(ParameterMappingResult {
                mapped_parameters: HashMap::new(),
                confidence: 1.0, // No mapping needed = perfect confidence
                missing_required: self.extract_required_params(tool_schema),
                type_conversions: Vec::new(),
                mapping_details: Vec::new(),
            });
        }

        // Build LLM prompt for semantic mapping
        let prompt = self
            .build_mapping_prompt(tool_schema, extracted_entities, query_text)
            .await?;

        // Call LLM for intelligent mapping
        let started_at = std::time::Instant::now();
        let llm_response = match self.llm_telemetry.as_ref() {
            Some((telemetry, _)) => {
                self.llm_service
                    .generate_analysis_scoped(telemetry.scope(), &prompt)
                    .await
            },
            None => self.llm_service.generate_analysis(&prompt).await,
        }
        .context("LLM entity mapping failed")?;
        // Parse LLM response with markdown stripping
        debug!(
            "[MAGICIAN-V2-STRATEGY] Raw LLM mapping response: {}",
            llm_response.content
        );

        // Strip common markdown wrappers that LLMs add
        let cleaned_response = llm_response
            .content
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        debug!(
            "[MAGICIAN-V2-STRATEGY] Cleaned LLM response: {}",
            cleaned_response
        );

        let parsed = serde_json::from_str::<EntityMappingResponse>(cleaned_response);
        if let Some((telemetry, attribution)) = self.llm_telemetry.as_ref() {
            let latency_ms = started_at.elapsed().as_millis() as u64;
            match parsed.as_ref() {
                Ok(_) => telemetry.emit_validated_success(
                    "entity_mapping",
                    &llm_response,
                    latency_ms,
                    attribution.clone(),
                    "entity_mapping_json",
                ),
                Err(error) => telemetry.emit_validation_failure(
                    "entity_mapping",
                    &llm_response,
                    latency_ms,
                    attribution.clone(),
                    "entity_mapping_json",
                    &error.to_string(),
                ),
            }
        }
        let mapping_response: EntityMappingResponse = parsed.unwrap_or_else(|e| {
            warn!(
                "[MAGICIAN-V2-STRATEGY] Failed to parse LLM mapping response: {}",
                e
            );
            warn!("[MAGICIAN-V2-STRATEGY] Response was: {}", cleaned_response);
            debug!("[MAGICIAN-V2-STRATEGY] Falling back to direct mapping");
            self.fallback_direct_mapping(tool_schema, extracted_entities)
        });

        // Convert to result format with type conversions
        self.build_mapping_result(tool_schema, mapping_response, extracted_entities)
    }

    /// Build LLM prompt for entity-to-parameter mapping
    async fn build_mapping_prompt(
        &self,
        tool_schema: &serde_json::Value,
        entities: &HashMap<String, String>,
        query: &str,
    ) -> Result<String> {
        // Extract tool parameters info
        let params_description = self.format_tool_parameters(tool_schema);
        let entities_description = self.format_entities(entities);

        // Build variables for prompt template
        let mut variables = HashMap::new();
        variables.insert("tool_parameters".to_string(), params_description);
        variables.insert("extracted_entities".to_string(), entities_description);
        variables.insert("query".to_string(), query.to_string());

        // Load and render prompt from PromptManager
        let prompt = self
            .prompt_manager
            .get_rendered_prompt(
                constants::names::ENTITY_MAPPING,
                constants::versions::ENTITY_MAPPING,
                variables,
            )
            .await
            .context("Failed to load entity mapping prompt")?;

        Ok(prompt)
    }

    /// Format tool parameters for prompt
    fn format_tool_parameters(&self, schema: &serde_json::Value) -> String {
        let mut output = String::new();

        if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
            let required_params = self.extract_required_params(schema);

            for (name, prop_schema) in properties {
                let param_type = prop_schema
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("any");

                let description = prop_schema
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("");

                let required_marker = if required_params.contains(name) {
                    " (required)"
                } else {
                    " (optional)"
                };

                output.push_str(&format!(
                    "- {} [{}{}]: {}\n",
                    name, param_type, required_marker, description
                ));
            }
        }

        if output.is_empty() {
            output.push_str("No parameters defined in schema\n");
        }

        output
    }

    /// Format entities for prompt
    fn format_entities(&self, entities: &HashMap<String, String>) -> String {
        let mut output = String::new();

        for (key, value) in entities {
            output.push_str(&format!("- {}: \"{}\"\n", key, value));
        }

        if output.is_empty() {
            output.push_str("No entities extracted\n");
        }

        output
    }

    /// Extract required parameter names from schema
    fn extract_required_params(&self, schema: &serde_json::Value) -> Vec<String> {
        let mut required = Vec::new();

        if let Some(required_array) = schema.get("required").and_then(|v| v.as_array()) {
            for item in required_array {
                if let Some(param_name) = item.as_str() {
                    required.push(param_name.to_string());
                }
            }
        }

        required
    }

    /// Build final mapping result with type conversions
    fn build_mapping_result(
        &self,
        tool_schema: &serde_json::Value,
        mapping_response: EntityMappingResponse,
        _original_entities: &HashMap<String, String>,
    ) -> Result<ParameterMappingResult> {
        let mut mapped_parameters = HashMap::new();
        let mut type_conversions = Vec::new();
        let mut mapping_details = Vec::new();

        // Get parameter types from schema
        let properties = tool_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .context("Tool schema missing properties")?;

        for mapping in mapping_response.mappings {
            let param_name = mapping.parameter_name.clone();

            // Get expected type from schema
            let expected_type = properties
                .get(&param_name)
                .and_then(|p| p.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("string");

            // Convert value to appropriate JSON type
            let converted_value = self.convert_to_json_type(&mapping.value, expected_type);

            // Determine actual from_type based on incoming value
            let from_type = match &mapping.value {
                serde_json::Value::String(_) => "string",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::Bool(_) => "boolean",
                serde_json::Value::Array(_) => "array",
                serde_json::Value::Object(_) => "object",
                serde_json::Value::Null => "null",
            };

            // Record type conversion if types differ
            if from_type != expected_type && expected_type != "string" {
                type_conversions.push(TypeConversion {
                    parameter: param_name.clone(),
                    from_type: from_type.to_string(),
                    to_type: expected_type.to_string(),
                    original_value: mapping.value.clone(),
                    converted_value: converted_value.clone(),
                });
            }

            mapped_parameters.insert(param_name.clone(), converted_value);

            mapping_details.push(MappingDetail {
                entity_key: mapping.entity_key,
                parameter_name: param_name,
                confidence: mapping.confidence,
                reasoning: mapping.reasoning,
            });
        }

        Ok(ParameterMappingResult {
            mapped_parameters,
            confidence: mapping_response.overall_confidence,
            missing_required: mapping_response.missing_required,
            type_conversions,
            mapping_details,
        })
    }

    /// Convert value to appropriate JSON type based on schema
    fn convert_to_json_type(
        &self,
        value: &serde_json::Value,
        target_type: &str,
    ) -> serde_json::Value {
        // If value is already non-string (object, array, etc), use it as-is
        if !value.is_string() {
            return value.clone();
        }

        // Extract string value for conversion
        let str_value = value.as_str().unwrap_or("");

        match target_type {
            "integer" | "int" => str_value
                .parse::<i64>()
                .map(serde_json::Value::from)
                .unwrap_or_else(|_| serde_json::Value::String(str_value.to_string())),
            "number" => str_value
                .parse::<f64>()
                .map(serde_json::Value::from)
                .unwrap_or_else(|_| serde_json::Value::String(str_value.to_string())),
            "boolean" | "bool" => {
                let lower = str_value.to_lowercase();
                let bool_val = matches!(lower.as_str(), "true" | "yes" | "1" | "on");
                serde_json::Value::Bool(bool_val)
            },
            "array" => {
                // Try to parse as JSON array, otherwise split by comma
                serde_json::from_str(str_value).unwrap_or_else(|_| {
                    let items: Vec<serde_json::Value> = str_value
                        .split(',')
                        .map(|s| serde_json::Value::String(s.trim().to_string()))
                        .collect();
                    serde_json::Value::Array(items)
                })
            },
            "object" => {
                // Try to parse as JSON object
                serde_json::from_str(str_value)
                    .unwrap_or_else(|_| serde_json::Value::String(str_value.to_string()))
            },
            _ => serde_json::Value::String(str_value.to_string()),
        }
    }

    /// Fallback to direct name matching if LLM fails
    fn fallback_direct_mapping(
        &self,
        tool_schema: &serde_json::Value,
        entities: &HashMap<String, String>,
    ) -> EntityMappingResponse {
        let mut mappings = Vec::new();

        if let Some(properties) = tool_schema.get("properties").and_then(|p| p.as_object()) {
            for (param_name, _) in properties {
                if let Some(entity_value) = entities.get(param_name) {
                    mappings.push(EntityMapping {
                        entity_key: param_name.clone(),
                        parameter_name: param_name.clone(),
                        value: serde_json::Value::String(entity_value.clone()),
                        confidence: 1.0, // Direct match = high confidence
                        reasoning: "Direct name match".to_string(),
                    });
                }
            }
        }

        let missing_required: Vec<String> = self
            .extract_required_params(tool_schema)
            .into_iter()
            .filter(|param| !entities.contains_key(param))
            .collect();

        let is_empty = mappings.is_empty();

        EntityMappingResponse {
            mappings,
            missing_required,
            overall_confidence: if is_empty { 0.0 } else { 0.8 },
        }
    }
}

// =============================================================================
// LLM RESPONSE TYPES
// =============================================================================

/// LLM response for entity mapping
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EntityMappingResponse {
    mappings: Vec<EntityMapping>,
    missing_required: Vec<String>,
    overall_confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EntityMapping {
    entity_key: String,
    parameter_name: String,
    value: serde_json::Value, // Changed from String to Value to handle objects
    confidence: f32,
    reasoning: String,
}

// =============================================================================
// PUBLIC RESULT TYPES
// =============================================================================

/// Result of entity-to-parameter mapping
#[derive(Debug, Clone)]
pub struct ParameterMappingResult {
    /// Successfully mapped parameters with proper types
    pub mapped_parameters: HashMap<String, serde_json::Value>,
    /// Overall confidence in the mapping (0.0-1.0)
    pub confidence: f32,
    /// Required parameters that could not be filled from entities
    pub missing_required: Vec<String>,
    /// Type conversions that were performed
    pub type_conversions: Vec<TypeConversion>,
    /// Detailed information about each mapping
    pub mapping_details: Vec<MappingDetail>,
}

/// Record of a type conversion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeConversion {
    pub parameter: String,
    pub from_type: String,
    pub to_type: String,
    pub original_value: serde_json::Value,
    pub converted_value: serde_json::Value,
}

/// Detailed information about a single mapping
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingDetail {
    pub entity_key: String,
    pub parameter_name: String,
    pub confidence: f32,
    pub reasoning: String,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse;

    /// Mock LLM for testing
    struct MockLLM {
        response: String,
    }

    #[async_trait]
    impl QueryAnalysisLLM for MockLLM {
        async fn generate_analysis(&self, _prompt: &str) -> Result<SimplifiedLLMResponse> {
            Ok(SimplifiedLLMResponse::content_only(self.response.clone()))
        }
    }

    #[tokio::test]
    async fn test_direct_name_match() {
        let mock_llm = Arc::new(MockLLM {
            response: r#"{
                "mappings": [
                    {
                        "entity_key": "target",
                        "parameter_name": "target",
                        "value": "google.com",
                        "confidence": 1.0,
                        "reasoning": "Direct name match"
                    }
                ],
                "missing_required": [],
                "overall_confidence": 1.0
            }"#
            .to_string(),
        }) as Arc<dyn QueryAnalysisLLM>;

        let prompt_manager = Arc::new(PromptManager::new(Arc::new(
            crate::magician_v2::prompts::JsonPromptStorage::new(
                crate::magician_v2::prompts::json_storage::JsonStorageConfig {
                    storage_dir: crate::magician_v2::prompts::json_storage::default_prompt_dir(),
                    enable_cache: false,
                    max_cache_entries: 1,
                },
            )
            .unwrap(),
        )));

        let mapper = EntityMapper::new(mock_llm, prompt_manager);

        let schema = json!({
            "properties": {
                "target": {"type": "string", "description": "Target hostname"}
            },
            "required": ["target"]
        });

        let mut entities = HashMap::new();
        entities.insert("target".to_string(), "google.com".to_string());

        let result = mapper
            .map_entities_to_parameters(&schema, &entities, "ping google.com")
            .await
            .unwrap();

        assert_eq!(result.mapped_parameters.len(), 1);
        assert_eq!(
            result.mapped_parameters.get("target"),
            Some(&json!("google.com"))
        );
        assert!(result.missing_required.is_empty());
    }

    #[tokio::test]
    async fn test_semantic_mapping() {
        let mock_llm = Arc::new(MockLLM {
            response: r#"{
                "mappings": [
                    {
                        "entity_key": "target",
                        "parameter_name": "hostname",
                        "value": "google.com",
                        "confidence": 0.95,
                        "reasoning": "Target entity maps semantically to hostname parameter"
                    }
                ],
                "missing_required": [],
                "overall_confidence": 0.95
            }"#
            .to_string(),
        }) as Arc<dyn QueryAnalysisLLM>;

        let prompt_manager = Arc::new(PromptManager::new(Arc::new(
            crate::magician_v2::prompts::JsonPromptStorage::new(
                crate::magician_v2::prompts::json_storage::JsonStorageConfig {
                    storage_dir: crate::magician_v2::prompts::json_storage::default_prompt_dir(),
                    enable_cache: false,
                    max_cache_entries: 1,
                },
            )
            .unwrap(),
        )));

        let mapper = EntityMapper::new(mock_llm, prompt_manager);

        let schema = json!({
            "properties": {
                "hostname": {"type": "string", "description": "Target hostname"}
            },
            "required": ["hostname"]
        });

        let mut entities = HashMap::new();
        entities.insert("target".to_string(), "google.com".to_string());

        let result = mapper
            .map_entities_to_parameters(&schema, &entities, "ping google.com")
            .await
            .unwrap();

        assert_eq!(result.mapped_parameters.len(), 1);
        assert_eq!(
            result.mapped_parameters.get("hostname"),
            Some(&json!("google.com"))
        );
    }

    #[tokio::test]
    async fn test_type_conversion() {
        let mock_llm = Arc::new(MockLLM {
            response: r#"{
                "mappings": [
                    {
                        "entity_key": "port",
                        "parameter_name": "port",
                        "value": "443",
                        "confidence": 1.0,
                        "reasoning": "Direct port mapping"
                    }
                ],
                "missing_required": [],
                "overall_confidence": 1.0
            }"#
            .to_string(),
        }) as Arc<dyn QueryAnalysisLLM>;

        let prompt_manager = Arc::new(PromptManager::new(Arc::new(
            crate::magician_v2::prompts::JsonPromptStorage::new(
                crate::magician_v2::prompts::json_storage::JsonStorageConfig {
                    storage_dir: crate::magician_v2::prompts::json_storage::default_prompt_dir(),
                    enable_cache: false,
                    max_cache_entries: 1,
                },
            )
            .unwrap(),
        )));

        let mapper = EntityMapper::new(mock_llm, prompt_manager);

        let schema = json!({
            "properties": {
                "port": {"type": "integer", "description": "Port number"}
            }
        });

        let mut entities = HashMap::new();
        entities.insert("port".to_string(), "443".to_string());

        let result = mapper
            .map_entities_to_parameters(&schema, &entities, "check port 443")
            .await
            .unwrap();

        // Should convert string "443" to integer 443
        assert_eq!(result.mapped_parameters.get("port"), Some(&json!(443)));
        assert_eq!(result.type_conversions.len(), 1);
        assert_eq!(result.type_conversions[0].to_type, "integer");
    }
}
