//! Configurable mock LLM service for testing.
//!
//! This module provides a unified mock LLM implementation that can be configured
//! to return different responses, capture requests for verification, and use
//! preset responses for common testing scenarios.
//!
//! # Example
//!
//! ```ignore
//! use crate::magician_v2::test_utils::{ConfigurableMockLlm, MockLlmPreset};
//!
//! // Use a preset for page understanding tests
//! let mock = ConfigurableMockLlm::with_preset(MockLlmPreset::PageUnderstanding);
//! let agent = PageUnderstandingAgent::new(Arc::new(mock), prompt_manager);
//!
//! // Use custom JSON response
//! let mock = ConfigurableMockLlm::with_response(r#"{"result": "custom"}"#);
//!
//! // Capture requests for verification
//! let (mock, captured) = ConfigurableMockLlm::capturing();
//! // ... run test ...
//! let request = captured.lock().unwrap().take().unwrap();
//! assert!(request.images.is_some());
//! ```

use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;

use crate::magician_v2::slot_graph::extraction::{
    LlmFunctionCallRequest, LlmFunctionCallResponse, LlmService,
};

/// Preset response configurations for common test scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockLlmPreset {
    /// Page understanding / vision analysis response
    PageUnderstanding,
    /// Action validation response
    Validation,
    /// Element location response
    ElementLocation,
    /// Slot extraction response
    SlotExtraction,
    /// Echo - returns user prompt as response (for debugging)
    Echo,
    /// Empty JSON object
    Empty,
}

impl MockLlmPreset {
    /// Get the JSON response for this preset.
    pub fn response(&self) -> &'static str {
        match self {
            MockLlmPreset::PageUnderstanding => PAGE_UNDERSTANDING_RESPONSE,
            MockLlmPreset::Validation => VALIDATION_RESPONSE,
            MockLlmPreset::ElementLocation => ELEMENT_LOCATION_RESPONSE,
            MockLlmPreset::SlotExtraction => SLOT_EXTRACTION_RESPONSE,
            MockLlmPreset::Echo => "", // Special case - handled in call_function
            MockLlmPreset::Empty => "{}",
        }
    }
}

// Preset response constants
const PAGE_UNDERSTANDING_RESPONSE: &str = r#"{
    "page_stage": "Dashboard",
    "elements": [
        {
            "element_type": "button",
            "text": "Sign In",
            "location": "center",
            "is_clickable": true,
            "confidence": 0.95
        },
        {
            "element_type": "input",
            "text": null,
            "location": "above button",
            "is_clickable": false,
            "confidence": 0.9
        }
    ],
    "layout_description": "Standard login form with username/password fields",
    "errors": [],
    "appears_loading": false,
    "confidence": 0.92,
    "observations": ["Clean modern design", "Two-factor auth option visible"]
}"#;

const VALIDATION_RESPONSE: &str = r#"{
    "succeeded": true,
    "confidence": 0.95,
    "reasoning": "Action completed successfully - expected element is now visible",
    "evidence": ["Element appeared after click", "Page title changed"],
    "suggestions": []
}"#;

const ELEMENT_LOCATION_RESPONSE: &str = r##"{
    "selector": "#submit-button",
    "alternatives": [
        "button[type='submit']",
        ".btn-primary",
        "[data-testid='submit']"
    ],
    "confidence": 0.9,
    "reasoning": "Found unique submit button with ID selector"
}"##;

const SLOT_EXTRACTION_RESPONSE: &str = r#"{
    "slots": [
        {
            "slot_type": "url",
            "value": {"url": "https://example.com"},
            "confidence": 0.95,
            "rationale": "Explicit URL provided in query"
        }
    ],
    "confidence": 0.9
}"#;

/// Login page response - used by vision/analyzer tests
/// Matches the exact format expected by analyzer tests (2 elements)
pub const LOGIN_PAGE_RESPONSE: &str = r#"{
    "page_stage": "Login",
    "elements": [
        {
            "element_type": "input",
            "text": null,
            "location": "center",
            "is_clickable": false,
            "confidence": 0.9
        },
        {
            "element_type": "button",
            "text": "Sign In",
            "location": "below inputs",
            "is_clickable": true,
            "confidence": 0.95
        }
    ],
    "layout_description": "Standard login form",
    "errors": [],
    "appears_loading": false,
    "confidence": 0.9,
    "observations": ["Clean design", "Two-factor auth option visible"]
}"#;

/// Content page response - used by vision/analyzer tests (capturing mock)
pub const CONTENT_PAGE_RESPONSE: &str = r#"{
    "page_stage": "Content",
    "elements": [],
    "layout_description": "test",
    "errors": [],
    "appears_loading": false,
    "confidence": 0.95,
    "observations": []
}"#;

/// Configurable mock LLM service for testing.
///
/// This mock can be configured to:
/// - Return a specific JSON response
/// - Use preset responses for common scenarios
/// - Capture requests for verification
/// - Echo back the user prompt (for debugging)
pub struct ConfigurableMockLlm {
    /// The response to return (JSON string)
    response: String,
    /// Whether to echo the user prompt instead of returning response
    echo_mode: bool,
    /// Optional request capture for verification
    captured_request: Option<Arc<Mutex<Option<LlmFunctionCallRequest>>>>,
    /// Provider name to return
    provider_name: String,
}

impl ConfigurableMockLlm {
    /// Create a mock with a custom JSON response.
    pub fn with_response(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
            echo_mode: false,
            captured_request: None,
            provider_name: "mock".to_string(),
        }
    }

    /// Create a mock using a preset response.
    pub fn with_preset(preset: MockLlmPreset) -> Self {
        if preset == MockLlmPreset::Echo {
            Self {
                response: String::new(),
                echo_mode: true,
                captured_request: None,
                provider_name: "mock-echo".to_string(),
            }
        } else {
            Self::with_response(preset.response())
        }
    }

    /// Create a mock that captures requests for later verification.
    ///
    /// Returns the mock and a shared reference to the captured request.
    pub fn capturing() -> (Self, Arc<Mutex<Option<LlmFunctionCallRequest>>>) {
        let captured = Arc::new(Mutex::new(None));
        let mock = Self {
            response: PAGE_UNDERSTANDING_RESPONSE.to_string(),
            echo_mode: false,
            captured_request: Some(captured.clone()),
            provider_name: "mock-capturing".to_string(),
        };
        (mock, captured)
    }

    /// Create a capturing mock with a specific preset response.
    pub fn capturing_with_preset(
        preset: MockLlmPreset,
    ) -> (Self, Arc<Mutex<Option<LlmFunctionCallRequest>>>) {
        let captured = Arc::new(Mutex::new(None));
        let mock = Self {
            response: preset.response().to_string(),
            echo_mode: preset == MockLlmPreset::Echo,
            captured_request: Some(captured.clone()),
            provider_name: "mock-capturing".to_string(),
        };
        (mock, captured)
    }

    /// Create a capturing mock with a custom response.
    ///
    /// Returns the mock and a shared reference to the captured request.
    pub fn capturing_with_response(
        response: impl Into<String>,
    ) -> (Self, Arc<Mutex<Option<LlmFunctionCallRequest>>>) {
        let captured = Arc::new(Mutex::new(None));
        let mock = Self {
            response: response.into(),
            echo_mode: false,
            captured_request: Some(captured.clone()),
            provider_name: "mock-capturing".to_string(),
        };
        (mock, captured)
    }

    /// Set a custom provider name.
    pub fn with_provider_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }
}

#[async_trait]
impl LlmService for ConfigurableMockLlm {
    async fn call_function(
        &self,
        request: LlmFunctionCallRequest,
    ) -> Result<LlmFunctionCallResponse> {
        // Capture request if configured
        if let Some(captured) = &self.captured_request {
            *captured.lock().unwrap() = Some(request.clone());
        }

        // Determine response
        let response = if self.echo_mode {
            request.user_prompt.clone()
        } else {
            self.response.clone()
        };

        Ok(LlmFunctionCallResponse {
            raw_arguments: response,
            telemetry: None,
        })
    }

    fn provider_name(&self) -> String {
        self.provider_name.clone()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::slot_graph::extraction::ImageData;

    #[tokio::test]
    async fn test_with_preset_page_understanding() {
        let mock = ConfigurableMockLlm::with_preset(MockLlmPreset::PageUnderstanding);

        let request = LlmFunctionCallRequest {
            system_prompt: "Analyze page".to_string(),
            user_prompt: "What's on this page?".to_string(),
            function_schema: "{}".to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: None,
        };

        let response = mock.call_function(request).await.unwrap();
        assert!(response.raw_arguments.contains("page_stage"));
        assert!(response.raw_arguments.contains("Dashboard"));
    }

    #[tokio::test]
    async fn test_with_preset_validation() {
        let mock = ConfigurableMockLlm::with_preset(MockLlmPreset::Validation);

        let request = LlmFunctionCallRequest {
            system_prompt: "Validate".to_string(),
            user_prompt: "Did the action work?".to_string(),
            function_schema: "{}".to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: None,
        };

        let response = mock.call_function(request).await.unwrap();
        assert!(response.raw_arguments.contains("succeeded"));
        assert!(response.raw_arguments.contains("true"));
    }

    #[tokio::test]
    async fn test_echo_mode() {
        let mock = ConfigurableMockLlm::with_preset(MockLlmPreset::Echo);

        let request = LlmFunctionCallRequest {
            system_prompt: "System".to_string(),
            user_prompt: "Echo this back".to_string(),
            function_schema: "{}".to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: None,
        };

        let response = mock.call_function(request).await.unwrap();
        assert_eq!(response.raw_arguments, "Echo this back");
    }

    #[tokio::test]
    async fn test_capturing_requests() {
        let (mock, captured) = ConfigurableMockLlm::capturing();

        let image = ImageData::new("base64data".to_string(), "image/png".to_string());
        let request = LlmFunctionCallRequest {
            system_prompt: "Analyze".to_string(),
            user_prompt: "What's here?".to_string(),
            function_schema: "{}".to_string(),
            model: "gpt-5".to_string(),
            temperature: 0.1,
            images: Some(vec![image]),
        };

        let _ = mock.call_function(request.clone()).await.unwrap();

        let captured_req = captured.lock().unwrap().take().unwrap();
        assert_eq!(captured_req.model, "gpt-5");
        assert!(captured_req.images.is_some());
        assert_eq!(captured_req.images.as_ref().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_custom_response() {
        let mock = ConfigurableMockLlm::with_response(r#"{"custom": "response"}"#);

        let request = LlmFunctionCallRequest {
            system_prompt: "".to_string(),
            user_prompt: "".to_string(),
            function_schema: "{}".to_string(),
            model: "test".to_string(),
            temperature: 0.0,
            images: None,
        };

        let response = mock.call_function(request).await.unwrap();
        assert!(response.raw_arguments.contains("custom"));
    }

    #[test]
    fn test_provider_name() {
        let mock = ConfigurableMockLlm::with_preset(MockLlmPreset::Empty)
            .with_provider_name("my-custom-provider");

        assert_eq!(mock.provider_name(), "my-custom-provider");
    }
}
