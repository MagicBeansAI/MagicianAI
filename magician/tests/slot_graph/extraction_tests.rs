use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use magician::magician_v2::{
    prompts::{constants, storage::PromptStore, types::PromptCategory, Prompt, PromptManager},
    slot_graph::{
        ExtractionConfig, LlmFunctionCallRequest, LlmFunctionCallResponse, LlmService,
        ProvisionalSlot, SlotExtractor, SlotType,
    },
};
use serde_json::json;

struct TestLlm {
    response: Mutex<Option<String>>,
}

impl TestLlm {
    fn new(response: &str) -> Self {
        Self {
            response: Mutex::new(Some(response.to_string())),
        }
    }
}

#[async_trait]
impl LlmService for TestLlm {
    async fn call_function(
        &self,
        _request: LlmFunctionCallRequest,
    ) -> Result<LlmFunctionCallResponse> {
        let mut guard = self.response.lock().expect("mutex poisoned");
        guard
            .take()
            .map(|raw_arguments| LlmFunctionCallResponse {
                raw_arguments,
                telemetry: None,
            })
            .ok_or_else(|| anyhow!("response already consumed"))
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
                "prompt '{}' v{} not available in StaticPromptStore",
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
        "integration-test".to_string(),
    );

    Arc::new(PromptManager::new(Arc::new(StaticPromptStore { prompt })))
}

#[tokio::test]
async fn extraction_pipeline_returns_slots() {
    let llm = Arc::new(TestLlm::new(
        r#"{"slots":[{"slot_type":"action","value":{"verb":"email"},"confidence":0.9,"rationale":"User asked to email"}]}"#,
    ));
    let prompt_manager = make_prompt_manager();
    let extractor = SlotExtractor::new(llm, prompt_manager, ExtractionConfig::default());

    let slots = extractor
        .extract_slots("Please email the client", None, None, None)
        .await
        .expect("extraction succeeds");

    assert_eq!(
        slots,
        vec![ProvisionalSlot {
            slot_type: SlotType::Action,
            value: json!({"verb": "email"}),
            confidence: 0.9,
            rationale: "User asked to email".to_string(),
        }]
    );
}
