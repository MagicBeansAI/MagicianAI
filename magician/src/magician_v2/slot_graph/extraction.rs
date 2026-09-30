use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::OnceCell;

use super::types::{ProvisionalSlot, SlotRecord};
use crate::magician_v2::{
    analytics::operation_llm_telemetry::{
        OperationLlmCallAttribution, OperationLlmTelemetryContext,
    },
    prompts::{constants, PromptManager},
};

/// Configuration for the [`SlotExtractor`].
#[derive(Debug, Clone)]
pub struct ExtractionConfig {
    /// Model identifier the downstream LLM client should use.
    pub model: String,
    /// Temperature for the extraction request.
    pub temperature: f64,
    /// Maximum slots that should be returned per request.
    pub max_slots_per_request: usize,
    /// Version of the slot extraction prompt to load from PromptManager.
    pub prompt_version: String,
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            model: "gpt-5.6-terra".to_string(),
            temperature: 0.0,
            max_slots_per_request: 12,
            prompt_version: constants::versions::SLOT_EXTRACTION.to_string(),
        }
    }
}

/// Summary of the surrounding conversation that can guide extraction.
#[derive(Debug, Clone, Default)]
pub struct ConversationContext {
    /// Prior messages that led to the current user request.
    pub previous_messages: Vec<Message>,
    /// Previously collected slots that might steer extraction.
    pub existing_slots: Vec<SlotRecord>,
    /// Placeholder for screenshots & vision metadata (Task 4.1.2).
    ///
    /// Vision processing is intentionally deferred — the extractor currently
    /// ignores this field and focuses on text-only extraction.
    pub screenshots: Vec<Screenshot>,
}

/// Lightweight representation of a conversation message.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: Option<DateTime<Utc>>,
}

/// Roles supported by the [`ConversationContext`].
#[derive(Debug, Clone, Copy)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Placeholder for vision data until multimodal extraction lands (Task 4.1.2).
#[derive(Debug, Clone, Default)]
pub struct Screenshot {
    pub image_data: Vec<u8>,
    pub timestamp: Option<DateTime<Utc>>,
    pub metadata: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct SlotExtractionPromptArtifacts {
    system_prompt: String,
    function_schema: String,
}

#[derive(Debug, Deserialize)]
struct SlotExtractionPromptDefinition {
    system_prompt: String,
    function_schema: serde_json::Value,
}

/// Thin interface that the extractor uses to call into an LLM client capable of
/// structured function-calling responses.
#[async_trait]
pub trait LlmService: Send + Sync {
    async fn call_function(
        &self,
        request: LlmFunctionCallRequest,
    ) -> Result<LlmFunctionCallResponse>;

    async fn call_function_scoped(
        &self,
        _scope: magicllm::LlmScope,
        request: LlmFunctionCallRequest,
    ) -> Result<LlmFunctionCallResponse> {
        self.call_function(request).await
    }

    /// Get the provider name for this LLM service (e.g., "openai", "anthropic", "ollama")
    fn provider_name(&self) -> String {
        "unknown".to_string()
    }
}

/// Detail level for vision model image analysis
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageDetail {
    /// Let the model decide the detail level automatically
    #[default]
    Auto,
    /// Lower detail - faster, cheaper, less accurate
    Low,
    /// Higher detail - slower, more expensive, more accurate
    High,
}

/// Image data for multimodal LLM requests
#[derive(Debug, Clone)]
pub struct ImageData {
    /// Base64-encoded image data (without data:image/png;base64, prefix)
    pub base64: String,

    /// MIME type (e.g., "image/png", "image/jpeg", "image/webp")
    pub media_type: String,

    /// Optional detail level for vision models
    /// - Auto: Let model decide (default)
    /// - Low: Faster, cheaper, less detail
    /// - High: Slower, more expensive, more detail
    pub detail: Option<ImageDetail>,
}

impl ImageData {
    /// Create a new ImageData instance
    pub fn new(base64: String, media_type: String) -> Self {
        Self {
            base64,
            media_type,
            detail: Some(ImageDetail::default()),
        }
    }

    /// Create ImageData with specific detail level
    pub fn with_detail(base64: String, media_type: String, detail: ImageDetail) -> Self {
        Self {
            base64,
            media_type,
            detail: Some(detail),
        }
    }
}

/// Request payload provided to the LLM service.
#[derive(Debug, Clone)]
pub struct LlmFunctionCallRequest {
    pub system_prompt: String,
    pub user_prompt: String,
    pub function_schema: String,
    pub model: String,
    pub temperature: f64,

    /// Optional images for multimodal requests
    /// Modern unified models (GPT-5, Sonnet 4.5) support multiple images
    pub images: Option<Vec<ImageData>>,
}

/// Per-call LLM telemetry. Carried through `LlmFunctionCallResponse` so the
/// executor can emit token / cache / cost data on `llm.succeeded` events.
///
/// Provider is stored as a lowercase string (`"anthropic"`, `"minimax"`,
/// `"openai"`, ...) to keep this struct free of cross-crate imports.
#[derive(Debug, Clone, Default)]
pub struct LlmCallTelemetry {
    pub provider: String,
    pub model: String,
    /// True only when these token buckets came from a provider usage object.
    /// Identity-only telemetry keeps this false so Phase 2 facts retain NULL
    /// usage instead of fabricating zero tokens and zero cost.
    pub usage_reported: bool,
    /// Optional aggregate-harness availability; absent preserves native pricing semantics.
    pub usage_availability: Option<magicllm::types::UsageAvailability>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub reasoning_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    /// Provider-executed web searches this call ran (`server_web_search`
    /// lane). Each bills per call on top of tokens; carried so canonical
    /// cost recomputation includes the charges. Zero for ordinary calls.
    pub search_calls: u32,
    pub cost_usd: f64,
    /// Provider-emitted reasoning / chain-of-thought summary text for this call,
    /// when the profile requested one. Carried into `LLMResponseReceived` so the
    /// reasoning lands in analytics, not just the token count. `None` when absent.
    pub reasoning_summary: Option<String>,
    /// Selected LLM profile name (e.g. "gptterra-responses-toolsany").
    /// Populated by the router when the call goes through `OperationLlmRouter`.
    pub profile: Option<String>,
    /// Typed LLM operation name (e.g. "agentic_decision", "agentic_decision_som").
    /// Populated by the router from `LLMOperation::as_str()`.
    pub operation: Option<String>,
    /// Start of the LLM call (Unix ms). Captured by the router before dispatch.
    pub started_at_ms: i64,
    /// Local-only stable logical-call and dispatch receipt.
    pub trace_receipt: Option<magicllm::LlmTraceReceipt>,
    /// Provider conversation projection selected for this call. Agentic
    /// decision code stamps this before response lowering so validation
    /// failures retain the same transport truth as successful decisions.
    pub prompt_projection_mode: Option<String>,
}

/// Response returned by the LLM service.
#[derive(Debug, Clone, Default)]
pub struct LlmFunctionCallResponse {
    pub raw_arguments: String,
    /// Populated when the underlying router can preserve call identity.
    /// `LlmCallTelemetry::usage_reported` separately distinguishes a real
    /// provider usage object from identity-only telemetry.
    pub telemetry: Option<LlmCallTelemetry>,
}

/// Primary component responsible for turning natural language into structured
/// slot candidates.
pub struct SlotExtractor {
    llm_service: Arc<dyn LlmService>,
    config: ExtractionConfig,
    prompt_manager: Arc<PromptManager>,
    prompt_assets: OnceCell<SlotExtractionPromptArtifacts>,
    event_broadcaster:
        Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
}

impl SlotExtractor {
    pub fn new(
        llm_service: Arc<dyn LlmService>,
        prompt_manager: Arc<PromptManager>,
        config: ExtractionConfig,
    ) -> Self {
        Self {
            llm_service,
            config,
            prompt_manager,
            prompt_assets: OnceCell::new(),
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

    /// Get the LLM service used by this extractor
    pub fn llm_service(&self) -> &Arc<dyn LlmService> {
        &self.llm_service
    }

    /// Get the configuration used by this extractor
    pub fn config(&self) -> &ExtractionConfig {
        &self.config
    }

    /// Extract provisional slots from the given message and optional context.
    pub async fn extract_slots(
        &self,
        user_message: &str,
        context: Option<&ConversationContext>,
        thread_id: Option<&str>,
        correlation_id: Option<&str>,
    ) -> Result<Vec<ProvisionalSlot>> {
        self.extract_slots_with_scope(
            user_message,
            context,
            thread_id,
            correlation_id,
            None,
            None,
            OperationLlmCallAttribution::default(),
        )
        .await
    }

    pub async fn extract_slots_with_scope(
        &self,
        user_message: &str,
        context: Option<&ConversationContext>,
        thread_id: Option<&str>,
        correlation_id: Option<&str>,
        principal: Option<&str>,
        workspace: Option<&str>,
        attribution: OperationLlmCallAttribution,
    ) -> Result<Vec<ProvisionalSlot>> {
        if user_message.trim().is_empty() {
            return Ok(vec![]);
        }

        let prompt = build_user_prompt(user_message, context);
        let artifacts = self.prompt_artifacts().await?;
        let request = LlmFunctionCallRequest {
            system_prompt: artifacts.system_prompt.clone(),
            user_prompt: prompt.clone(),
            function_schema: artifacts.function_schema.clone(),
            model: self.config.model.clone(),
            temperature: self.config.temperature,
            images: None,
        };

        // Emit LLM analysis started event
        if let (Some(tid), Some(cid), Some(ref broadcaster)) =
            (thread_id, correlation_id, &self.event_broadcaster)
        {
            broadcaster.llm_analysis_started(
                tid,
                cid,
                self.llm_service.provider_name(),
                "slot_extraction".to_string(),
                prompt.len(),
            );
        }

        let start = std::time::Instant::now();
        let llm_call = match (principal, workspace) {
            (Some(principal), Some(workspace)) => self
                .llm_service
                .call_function_scoped(magicllm::LlmScope::new(principal, workspace), request),
            _ => self.llm_service.call_function(request),
        };
        let response = match llm_call.await {
            Ok(resp) => {
                let duration = start.elapsed();

                // Emit LLM analysis completed event
                if let (Some(tid), Some(cid), Some(ref broadcaster)) =
                    (thread_id, correlation_id, &self.event_broadcaster)
                {
                    broadcaster.llm_analysis_completed(
                        tid,
                        cid,
                        self.llm_service.provider_name(),
                        "slot_extraction".to_string(),
                        resp.raw_arguments.len(),
                        duration.as_millis() as u64,
                    );
                }
                resp
            },
            Err(e) => {
                // Emit LLM analysis failed event
                if let (Some(tid), Some(cid), Some(ref broadcaster)) =
                    (thread_id, correlation_id, &self.event_broadcaster)
                {
                    let error_type = if e.to_string().contains("timeout") {
                        "timeout"
                    } else if e.to_string().contains("api_key") || e.to_string().contains("API key")
                    {
                        "api_key_missing"
                    } else if e.to_string().contains("network") {
                        "network_error"
                    } else {
                        "invalid_response"
                    };

                    broadcaster.llm_analysis_failed(
                        tid,
                        cid,
                        self.llm_service.provider_name(),
                        "slot_extraction".to_string(),
                        error_type.to_string(),
                        e.to_string(),
                    );
                }

                return Err(e);
            },
        };
        let parsed = parse_function_response(&response.raw_arguments)
            .context("failed to parse extraction function response");
        if let (Some(principal), Some(workspace), Some(broadcaster), Some(usage)) = (
            principal,
            workspace,
            self.event_broadcaster.as_ref(),
            response.telemetry.as_ref(),
        ) {
            let telemetry = OperationLlmTelemetryContext::new(
                Arc::clone(broadcaster),
                principal,
                workspace,
                "slot_extraction",
            );
            let latency_ms = start.elapsed().as_millis() as u64;
            match parsed.as_ref() {
                Ok(_) => telemetry.emit_usage_validated_success(
                    "slot_extraction",
                    usage,
                    latency_ms,
                    attribution,
                    "slot_extraction_function",
                ),
                Err(error) => telemetry.emit_usage_validation_failure(
                    "slot_extraction",
                    usage,
                    latency_ms,
                    attribution,
                    "slot_extraction_function",
                    &error.to_string(),
                ),
            }
        }
        let mut provisional_slots = parsed?;

        provisional_slots.truncate(self.config.max_slots_per_request);
        let provisional_slots = provisional_slots
            .into_iter()
            .map(ProvisionalSlot::validate)
            .filter_map(|slot| match slot {
                Ok(slot) => Some(slot),
                Err(err) => {
                    tracing::warn!("[MAGICIAN-V2-SLOT] Dropping invalid provisional slot: {err:?}");
                    None
                },
            })
            .collect();

        Ok(provisional_slots)
    }

    async fn prompt_artifacts(&self) -> Result<&SlotExtractionPromptArtifacts> {
        self.prompt_assets
            .get_or_try_init(|| async {
                let prompt = self
                    .prompt_manager
                    .get_prompt(
                        constants::names::SLOT_EXTRACTION,
                        &self.config.prompt_version,
                    )
                    .await
                    .context("failed to load slot extraction prompt")?;

                let definition: SlotExtractionPromptDefinition =
                    serde_json::from_str(&prompt.content)
                        .context("slot extraction prompt content is not valid JSON")?;

                let function_schema = serde_json::to_string(&definition.function_schema)
                    .context("failed to serialize slot extraction function schema")?;

                Ok(SlotExtractionPromptArtifacts {
                    system_prompt: definition.system_prompt,
                    function_schema,
                })
            })
            .await
    }
}

fn build_user_prompt(user_message: &str, context: Option<&ConversationContext>) -> String {
    if let Some(context) = context {
        let mut prompt = String::new();
        let _ = writeln!(prompt, "User request:\n\"{}\"\n", user_message.trim());

        if !context.previous_messages.is_empty() {
            let _ = writeln!(prompt, "Conversation history (most recent last):");
            for message in &context.previous_messages {
                let role = match message.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                };
                let _ = writeln!(prompt, "- {}: {}", role, message.content.trim());
            }
            prompt.push('\n');
        }

        if !context.existing_slots.is_empty() {
            let _ = writeln!(prompt, "Existing slots already captured:");
            for slot in &context.existing_slots {
                let _ = writeln!(
                    prompt,
                    "- {:?} (confidence {:.2}): {}",
                    slot.slot_type, slot.confidence, slot.value
                );
            }
            prompt.push('\n');
        }

        prompt.push_str("Extract new slots that add information beyond the existing list.");
        prompt
    } else {
        format!(
            "User request:\n\"{}\"\n\nExtract all relevant slots from this request.",
            user_message.trim()
        )
    }
}

fn parse_function_response(raw_arguments: &str) -> Result<Vec<ProvisionalSlot>> {
    #[derive(Debug, Deserialize)]
    struct ExtractionPayload {
        #[serde(default)]
        slots: Vec<ProvisionalSlot>,
    }

    let payload: ExtractionPayload =
        serde_json::from_str(raw_arguments).context("invalid JSON returned by LLM")?;
    Ok(payload.slots)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Only the test module needs these; importing them at file scope would
    // re-introduce the unused-import warnings the crate split cleaned up.
    use super::super::types::SlotType;
    use anyhow::anyhow;

    use crate::magician_v2::prompts::{
        constants, storage::PromptStore, types::PromptCategory, Prompt, PromptManager,
    };

    struct MockLlmService {
        responses: Mutex<Vec<String>>,
    }

    impl MockLlmService {
        fn new(responses: Vec<String>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl LlmService for MockLlmService {
        async fn call_function(
            &self,
            _request: LlmFunctionCallRequest,
        ) -> Result<LlmFunctionCallResponse> {
            let mut guard = self
                .responses
                .lock()
                .expect("mock responses mutex poisoned");
            guard
                .pop()
                .map(|raw_arguments| LlmFunctionCallResponse {
                    raw_arguments,
                    telemetry: None,
                })
                .ok_or_else(|| anyhow!("no mock response available"))
        }
    }

    struct StaticPromptStore {
        prompt: Prompt,
    }

    #[async_trait]
    impl PromptStore for StaticPromptStore {
        async fn get_prompt(&self, name: &str, version: &str) -> Result<Prompt> {
            if name == self.prompt.name && version == self.prompt.version {
                Ok(self.prompt.clone())
            } else {
                Err(anyhow!(
                    "prompt '{}' version '{}' not found in StaticPromptStore",
                    name,
                    version
                ))
            }
        }

        async fn list_versions(&self, name: &str) -> Result<Vec<String>> {
            if name == self.prompt.name {
                Ok(vec![self.prompt.version.clone()])
            } else {
                Ok(vec![])
            }
        }

        async fn list_prompt_names(&self) -> Result<Vec<String>> {
            Ok(vec![self.prompt.name.clone()])
        }

        async fn save_prompt(&self, _prompt: &Prompt) -> Result<()> {
            Ok(())
        }

        async fn prompt_exists(&self, name: &str, version: &str) -> Result<bool> {
            Ok(name == self.prompt.name && version == self.prompt.version)
        }

        async fn latest_version(&self, name: &str) -> Result<String> {
            if name == self.prompt.name {
                Ok(self.prompt.version.clone())
            } else {
                Err(anyhow!("prompt '{}' not found in StaticPromptStore", name))
            }
        }

        async fn delete_prompt(&self, _name: &str, _version: &str) -> Result<()> {
            Ok(())
        }

        async fn initialize(&self) -> Result<()> {
            Ok(())
        }

        async fn health_check(&self) -> Result<bool> {
            Ok(true)
        }
    }

    fn make_prompt_manager() -> Arc<PromptManager> {
        let content = serde_json::json!({
            "system_prompt": "You are an information extraction assistant. Extract all relevant structured information.",
            "function_schema": {
                "name": "extract_slots",
                "description": "Extract structured information from user request",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "slots": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "slot_type": {
                                        "type": "string",
                                        "enum": [
                                            "entity",
                                            "temporal",
                                            "spatial",
                                            "emotion",
                                            "action",
                                            "modifier",
                                            "resource",
                                            "status"
                                        ]
                                    },
                                    "value": {
                                        "type": "object"
                                    },
                                    "confidence": {
                                        "type": "number",
                                        "minimum": 0.0,
                                        "maximum": 1.0
                                    },
                                    "rationale": {
                                        "type": "string"
                                    }
                                },
                                "required": [
                                    "slot_type",
                                    "value",
                                    "confidence",
                                    "rationale"
                                ]
                            }
                        }
                    }
                }
            }
        })
        .to_string();

        let prompt = Prompt::new(
            constants::names::SLOT_EXTRACTION.to_string(),
            constants::versions::SLOT_EXTRACTION.to_string(),
            content,
            PromptCategory::General,
            "Test slot extraction prompt".to_string(),
            "unit-test".to_string(),
        );

        Arc::new(PromptManager::new(Arc::new(StaticPromptStore { prompt })))
    }

    fn make_slot_record(
        slot_type: SlotType,
        value: serde_json::Value,
        confidence: f64,
    ) -> SlotRecord {
        SlotRecord {
            id: "id".to_string(),
            slot_type,
            value,
            confidence,
            provenance: vec![],
            evidence_links: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn extract_slots_returns_parsed_slots() {
        let llm = Arc::new(MockLlmService::new(vec![r#"{
            "slots": [{
                "slot_type": "entity",
                "value": {"type": "person", "name": "Ada"},
                "confidence": 0.85,
                "rationale": "User mentioned Ada explicitly"
            }]
        }"#
        .to_string()]));
        let prompt_manager = make_prompt_manager();
        let extractor = SlotExtractor::new(llm, prompt_manager, ExtractionConfig::default());
        let slots = extractor
            .extract_slots("Follow up with Ada tomorrow", None, None, None)
            .await
            .unwrap();

        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].slot_type, SlotType::Entity);
        assert_eq!(
            slots[0].value,
            serde_json::json!({"type": "person", "name": "Ada"})
        );
        assert!((slots[0].confidence - 0.85).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn extract_slots_respects_max_slots() {
        let llm = Arc::new(MockLlmService::new(vec![r#"{
            "slots": [
                {"slot_type": "entity", "value": {"name": "A"}, "confidence": 0.7, "rationale": "a"},
                {"slot_type": "entity", "value": {"name": "B"}, "confidence": 0.7, "rationale": "b"},
                {"slot_type": "entity", "value": {"name": "C"}, "confidence": 0.7, "rationale": "c"}
            ]
        }"#.to_string()]));
        let prompt_manager = make_prompt_manager();
        let extractor = SlotExtractor::new(
            llm,
            prompt_manager,
            ExtractionConfig {
                model: "test".into(),
                temperature: 0.0,
                max_slots_per_request: 2,
                prompt_version: constants::versions::SLOT_EXTRACTION.to_string(),
            },
        );

        let slots = extractor
            .extract_slots("Find contacts A, B, and C", None, None, None)
            .await
            .unwrap();
        assert_eq!(slots.len(), 2);
    }

    #[tokio::test]
    async fn extract_slots_ignores_invalid_entries() {
        let llm = Arc::new(MockLlmService::new(vec![r#"{
            "slots": [
                {"slot_type": "entity", "value": {"name": "Valid"}, "confidence": 0.8, "rationale": "fine"},
                {"slot_type": "entity", "value": "oops", "confidence": 5.0, "rationale": ""}
            ]
        }"#.to_string()]));
        let prompt_manager = make_prompt_manager();
        let extractor = SlotExtractor::new(llm, prompt_manager, ExtractionConfig::default());
        let slots = extractor
            .extract_slots("Need info", None, None, None)
            .await
            .unwrap();

        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].value, serde_json::json!({"name": "Valid"}));
    }

    #[tokio::test]
    async fn extract_slots_handles_context_prompting() {
        let llm = Arc::new(MockLlmService::new(vec![r#"{
            "slots": [{
                "slot_type": "temporal",
                "value": {"type": "date", "value": "2025-02-01"},
                "confidence": 0.6,
                "rationale": "Explicit date mentioned"
            }]
        }"#
        .to_string()]));
        let prompt_manager = make_prompt_manager();

        let context = ConversationContext {
            previous_messages: vec![
                Message {
                    role: MessageRole::Assistant,
                    content: "Sure, I can help with scheduling.".into(),
                    timestamp: None,
                },
                Message {
                    role: MessageRole::User,
                    content: "Let's meet in February.".into(),
                    timestamp: None,
                },
            ],
            existing_slots: vec![make_slot_record(
                SlotType::Entity,
                serde_json::json!({"name": "Ada"}),
                0.9,
            )],
            screenshots: vec![],
        };

        let extractor = SlotExtractor::new(llm, prompt_manager, ExtractionConfig::default());
        let slots = extractor
            .extract_slots("Confirm for February 1st", Some(&context), None, None)
            .await
            .unwrap();

        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].slot_type, SlotType::Temporal);
    }
}
