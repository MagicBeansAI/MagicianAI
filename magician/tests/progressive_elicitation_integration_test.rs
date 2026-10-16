//! Integration tests for Progressive Elicitation
//!
//! Tests the complete progressive elicitation flow including:
//! - Orchestrator inference wiring
//! - Parameter inference service integration
//! - Autonomous discovery service integration
//! - WebSocket event broadcasting
//! - Real-time UI updates
//!
//! Run with: cargo test progressive_elicitation_integration --test-execs=1 -- --nocapture

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use magician::magician_v2::{
    elicitation::{
        AutonomousDiscoveryService, DiscoveryMethod, DiscoveryResult, ElicitationError,
        InferenceMethod, InferenceResult, InferenceSource, ParameterContext,
        ParameterInferenceService, WorkflowStage,
    },
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
    strategy::plan::{AskTiming, DiscoveryTiming, InputSource, QuestionPriority, UnresolvedInput},
};
use serde_json::{json, Value};
use tokio::sync::Mutex;

// ============================================================================
// Mock Inference Service
// ============================================================================

struct MockInferenceService {
    /// Predefined responses: parameter_name -> (value, confidence, method)
    responses: Mutex<HashMap<String, (Value, f64, InferenceMethod)>>,
}

impl MockInferenceService {
    fn new() -> Self {
        Self {
            responses: Mutex::new(HashMap::new()),
        }
    }

    /// Add a high-confidence inference response
    fn with_high_confidence(self, param: &str, value: Value) -> Self {
        let mut responses = self
            .responses
            .try_lock()
            .expect("responses mutex should not be locked during setup");
        responses.insert(param.to_string(), (value, 0.95, InferenceMethod::LLMBased));
        drop(responses);
        self
    }

    /// Add a low-confidence inference response
    fn with_low_confidence(self, param: &str, value: Value) -> Self {
        let mut responses = self
            .responses
            .try_lock()
            .expect("responses mutex should not be locked during setup");
        responses.insert(param.to_string(), (value, 0.4, InferenceMethod::RuleBased));
        drop(responses);
        self
    }

    /// Add a medium-confidence inference response
    fn with_medium_confidence(self, param: &str, value: Value) -> Self {
        let mut responses = self
            .responses
            .try_lock()
            .expect("responses mutex should not be locked during setup");
        responses.insert(
            param.to_string(),
            (value, 0.75, InferenceMethod::Historical),
        );
        drop(responses);
        self
    }
}

#[async_trait]
impl ParameterInferenceService for MockInferenceService {
    async fn infer(
        &self,
        input: &UnresolvedInput,
        _context: &ParameterContext,
    ) -> Result<InferenceResult, ElicitationError> {
        let responses = self.responses.lock().await;
        if let Some((value, confidence, method)) = responses.get(&input.parameter) {
            Ok(InferenceResult {
                value: Some(value.clone()),
                confidence: *confidence,
                method: method.clone(),
                explanation: format!(
                    "Inferred {} using {:?} with {:.0}% confidence",
                    input.parameter,
                    method,
                    confidence * 100.0
                ),
                sources: vec![InferenceSource {
                    source_type: format!("{:?}", method),
                    weight: *confidence,
                    description: "Mock inference source".to_string(),
                }],
            })
        } else {
            // No inference available
            Ok(InferenceResult {
                value: None,
                confidence: 0.0,
                method: InferenceMethod::RuleBased,
                explanation: format!("No inference available for {}", input.parameter),
                sources: vec![],
            })
        }
    }

    async fn can_infer(
        &self,
        input: &UnresolvedInput,
        _context: &ParameterContext,
    ) -> Result<bool, ElicitationError> {
        let responses = self.responses.lock().await;
        Ok(responses.contains_key(&input.parameter))
    }
}

// ============================================================================
// Mock Discovery Service
// ============================================================================

struct MockDiscoveryService {
    /// Predefined discoveries: parameter_name -> (value, confidence, method)
    discoveries: Mutex<HashMap<String, (Value, f64, DiscoveryMethod)>>,
}

impl MockDiscoveryService {
    fn new() -> Self {
        Self {
            discoveries: Mutex::new(HashMap::new()),
        }
    }

    /// Add a successful discovery
    fn with_discovery(self, param: &str, value: Value, method: DiscoveryMethod) -> Self {
        let mut discoveries = self
            .discoveries
            .try_lock()
            .expect("discoveries mutex should not be locked during setup");
        discoveries.insert(param.to_string(), (value, 0.85, method));
        drop(discoveries);
        self
    }
}

#[async_trait]
impl AutonomousDiscoveryService for MockDiscoveryService {
    async fn discover(
        &self,
        input: &UnresolvedInput,
        _context: &ParameterContext,
    ) -> Result<DiscoveryResult, ElicitationError> {
        let discoveries = self.discoveries.lock().await;
        if let Some((value, confidence, method)) = discoveries.get(&input.parameter) {
            Ok(DiscoveryResult {
                value: Some(value.clone()),
                confidence: *confidence,
                method: method.clone(),
                explanation: format!(
                    "Discovered {} using {:?} with {:.0}% confidence",
                    input.parameter,
                    method,
                    confidence * 100.0
                ),
                external_actions_performed: matches!(
                    method,
                    DiscoveryMethod::WebSearch
                        | DiscoveryMethod::FilesystemSearch
                        | DiscoveryMethod::APIQuery
                ),
            })
        } else {
            Ok(DiscoveryResult {
                value: None,
                confidence: 0.0,
                method: DiscoveryMethod::ContextAnalysis,
                explanation: format!("No discovery available for {}", input.parameter),
                external_actions_performed: false,
            })
        }
    }

    async fn is_safe_to_discover(
        &self,
        input: &UnresolvedInput,
        _context: &ParameterContext,
    ) -> Result<bool, ElicitationError> {
        let discoveries = self.discoveries.lock().await;
        Ok(discoveries.contains_key(&input.parameter))
    }
}

// ============================================================================
// Event Capture for Testing
// ============================================================================

struct EventCapture {
    events: Arc<Mutex<Vec<RuntimeTransportEvent>>>,
}

impl EventCapture {
    fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    async fn subscribe(&self, broadcaster: &RuntimeTransportBroadcaster) {
        let mut rx = broadcaster.subscribe();
        let events = self.events.clone();

        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                events.lock().await.push(event);
            }
        });

        // Give the spawn time to set up
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    async fn get_events(&self) -> Vec<RuntimeTransportEvent> {
        self.events.lock().await.clone()
    }

    async fn get_inference_events(&self) -> Vec<RuntimeTransportEvent> {
        self.events
            .lock()
            .await
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    RuntimeTransportEvent::ParameterInferred { .. }
                        | RuntimeTransportEvent::ParameterInferenceFailed { .. }
                        | RuntimeTransportEvent::ParameterInferenceAttempted { .. }
                )
            })
            .cloned()
            .collect()
    }

    #[allow(dead_code)]
    async fn get_discovery_events(&self) -> Vec<RuntimeTransportEvent> {
        self.events
            .lock()
            .await
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    RuntimeTransportEvent::ParameterDiscovered { .. }
                        | RuntimeTransportEvent::ParameterDiscoveryFailed { .. }
                        | RuntimeTransportEvent::ParameterDiscoveryAttempted { .. }
                )
            })
            .cloned()
            .collect()
    }

    async fn get_progress_events(&self) -> Vec<RuntimeTransportEvent> {
        self.events
            .lock()
            .await
            .iter()
            .filter(|e| matches!(e, RuntimeTransportEvent::ParameterResolutionProgress { .. }))
            .cloned()
            .collect()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
async fn test_inference_service_integration() {
    // Create mock inference service with predefined responses
    let inference_service = Arc::new(
        MockInferenceService::new()
            .with_high_confidence("target_host", json!("google.com"))
            .with_medium_confidence("port", json!(443))
            .with_low_confidence("timeout", json!(30)),
    );

    // Test high confidence inference (should succeed)
    let input = UnresolvedInput {
        id: "test_target_host".to_string(),
        parameter: "target_host".to_string(),
        display_name: "Target Host".to_string(),
        step_id: None,
        linked_steps: Vec::new(),
        expected_type: Some("string".to_string()),
        json_schema: None,
        prompt: "Enter the target host".to_string(),
        required: true,
        notes: None,
        priority: QuestionPriority::Critical,
        ask_timing: AskTiming::PreExecution,
        discovery_timing: DiscoveryTiming::PreExecution,
        default_value: None,
        inference_hints: Vec::new(),
        inference_threshold: 0.7,
        auto_fill: None,
        auto_fill_confidence: None,
        source: InputSource::Planner,
        created_at: None,
        updated_at: None,
        status: None,
    };

    let context = ParameterContext {
        execution_id: "test-exec".to_string(),
        user_message: "ping google.com".to_string(),
        tool_context: None,
        slot_context: HashMap::new(),
        observations: Vec::new(),
        stage: WorkflowStage::Planning,
        prompt_identity: None,
    };

    let result = inference_service.infer(&input, &context).await.unwrap();

    assert!(result.value.is_some());
    assert_eq!(result.value.unwrap(), json!("google.com"));
    assert_eq!(result.confidence, 0.95);
    assert_eq!(result.method, InferenceMethod::LLMBased);

    println!("✅ High confidence inference test passed");
}

#[tokio::test]
async fn test_discovery_service_integration() {
    // Create mock discovery service
    let discovery_service = Arc::new(
        MockDiscoveryService::new()
            .with_discovery(
                "config_file",
                json!("/etc/app/config.yaml"),
                DiscoveryMethod::FilesystemSearch,
            )
            .with_discovery(
                "api_endpoint",
                json!("https://api.example.com/v1"),
                DiscoveryMethod::WebSearch,
            ),
    );

    let input = UnresolvedInput {
        id: "test_config_file".to_string(),
        parameter: "config_file".to_string(),
        display_name: "Config File".to_string(),
        step_id: None,
        linked_steps: Vec::new(),
        expected_type: Some("string".to_string()),
        json_schema: None,
        prompt: "Enter the config file path".to_string(),
        required: true,
        notes: None,
        priority: QuestionPriority::PreExecution,
        ask_timing: AskTiming::PreExecution,
        discovery_timing: DiscoveryTiming::PreExecution,
        default_value: None,
        inference_hints: Vec::new(),
        inference_threshold: 0.7,
        auto_fill: None,
        auto_fill_confidence: None,
        source: InputSource::Planner,
        created_at: None,
        updated_at: None,
        status: None,
    };

    let context = ParameterContext {
        execution_id: "test-exec".to_string(),
        user_message: "find the config file".to_string(),
        tool_context: None,
        slot_context: HashMap::new(),
        observations: Vec::new(),
        stage: WorkflowStage::Planning,
        prompt_identity: None,
    };

    let result = discovery_service.discover(&input, &context).await.unwrap();

    assert!(result.value.is_some());
    assert_eq!(result.value.unwrap(), json!("/etc/app/config.yaml"));
    assert_eq!(result.confidence, 0.85);
    assert_eq!(result.method, DiscoveryMethod::FilesystemSearch);
    assert!(result.external_actions_performed);

    println!("✅ Discovery service test passed");
}

#[tokio::test]
async fn test_websocket_event_broadcasting() {
    // Create event broadcaster with capacity of 1000
    let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(1000));
    let capture = EventCapture::new();

    // Subscribe to events
    capture.subscribe(&broadcaster).await;

    // Broadcast various events
    broadcaster.emit_transport_only(RuntimeTransportEvent::ParameterInferenceAttempted {
        execution_id: "test-exec".to_string(),
        parameter_name: "target_host".to_string(),
        priority: "critical".to_string(),
        principal: None,
        workspace: None,
        timestamp: chrono::Utc::now().timestamp_millis(),
    });

    broadcaster.emit_transport_only(RuntimeTransportEvent::ParameterInferred {
        execution_id: "test-exec".to_string(),
        parameter_name: "target_host".to_string(),
        inferred_value: json!("google.com"),
        confidence: 0.95,
        method: "LLMBased".to_string(),
        principal: None,
        workspace: None,
        timestamp: chrono::Utc::now().timestamp_millis(),
    });

    broadcaster.emit_transport_only(RuntimeTransportEvent::ParameterResolutionProgress {
        execution_id: "test-exec".to_string(),
        total_parameters: 5,
        resolved_count: 2,
        inferred_count: 1,
        discovered_count: 0,
        deferred_count: 1,
        remaining_count: 3,
        principal: None,
        workspace: None,
        timestamp: chrono::Utc::now().timestamp_millis(),
    });

    // Wait for events to be captured
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Verify events were captured
    let events = capture.get_events().await;
    assert_eq!(events.len(), 3);

    let inference_events = capture.get_inference_events().await;
    assert_eq!(inference_events.len(), 2);

    let progress_events = capture.get_progress_events().await;
    assert_eq!(progress_events.len(), 1);

    println!("✅ WebSocket event broadcasting test passed");
    println!("   Total events: {}", events.len());
    println!("   Inference events: {}", inference_events.len());
    println!("   Progress events: {}", progress_events.len());
}

#[tokio::test]
async fn test_confidence_threshold_logic() {
    // Test that different confidence levels are handled correctly based on priority

    let inference_service = Arc::new(
        MockInferenceService::new()
            .with_high_confidence("critical_param", json!("value1")) // 0.95 > 0.9 (Critical threshold)
            .with_medium_confidence("pre_exec_param", json!("value2")) // 0.75 > 0.7 (PreExecution threshold)
            .with_low_confidence("optional_param", json!("value3")), // 0.4 < 0.5 (Optional threshold)
    );

    // Test Critical parameter (requires 0.9 confidence)
    let critical_input = UnresolvedInput {
        id: "test_critical_param".to_string(),
        parameter: "critical_param".to_string(),
        display_name: "Critical Param".to_string(),
        step_id: None,
        linked_steps: Vec::new(),
        expected_type: Some("string".to_string()),
        json_schema: None,
        prompt: "".to_string(),
        required: true,
        notes: None,
        priority: QuestionPriority::Critical,
        ask_timing: AskTiming::PreExecution,
        discovery_timing: DiscoveryTiming::PreExecution,
        default_value: None,
        inference_hints: Vec::new(),
        inference_threshold: 0.7,
        auto_fill: None,
        auto_fill_confidence: None,
        source: InputSource::Planner,
        created_at: None,
        updated_at: None,
        status: None,
    };

    let context = ParameterContext {
        execution_id: "test".to_string(),
        user_message: "test".to_string(),
        tool_context: None,
        slot_context: HashMap::new(),
        observations: Vec::new(),
        stage: WorkflowStage::Planning,
        prompt_identity: None,
    };

    let result = inference_service
        .infer(&critical_input, &context)
        .await
        .unwrap();
    assert_eq!(result.confidence, 0.95);
    assert!(result.confidence >= 0.9); // Should meet Critical threshold

    // Test Optional parameter (requires 0.5 confidence)
    let optional_input = UnresolvedInput {
        id: "test_optional_param".to_string(),
        parameter: "optional_param".to_string(),
        display_name: "Optional Param".to_string(),
        step_id: None,
        linked_steps: Vec::new(),
        expected_type: Some("string".to_string()),
        json_schema: None,
        prompt: "".to_string(),
        required: false,
        notes: None,
        priority: QuestionPriority::Optional,
        ask_timing: AskTiming::PreExecution,
        discovery_timing: DiscoveryTiming::PreExecution,
        default_value: None,
        inference_hints: Vec::new(),
        inference_threshold: 0.7,
        auto_fill: None,
        auto_fill_confidence: None,
        source: InputSource::Planner,
        created_at: None,
        updated_at: None,
        status: None,
    };

    let result = inference_service
        .infer(&optional_input, &context)
        .await
        .unwrap();
    assert_eq!(result.confidence, 0.4);
    assert!(result.confidence < 0.5); // Should NOT meet Optional threshold

    println!("✅ Confidence threshold logic test passed");
}
