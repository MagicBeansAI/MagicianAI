//! Integration tests for lowering → execution → observation pipeline
//!
//! These tests verify the complete execution flow from plan graphs to observable results.

use std::collections::HashMap;

use magician::magician_v2::{
    execution::{
        execute_bash_action, execute_file_action, execute_http_action, is_file_tool, is_http_tool,
        is_shell_tool, lower_plan_to_executable_steps, lower_step_to_executable_action,
        ExecutableAction, FileAction, PlanExecutionContext, PlanStatus,
    },
    strategy::plan::{PlanGraph, PlanProvenance, PlanStep},
};
use runtime_core::{FileSandboxConfig, FileSandboxMode, ShellSandboxConfig};

fn unrestricted_file_sandbox() -> FileSandboxConfig {
    FileSandboxConfig {
        mode: FileSandboxMode::Unrestricted,
        allowed_roots: vec![],
        allow_delete: true,
    }
}

fn make_test_step(id: &str, tool: &str, params: HashMap<String, String>) -> PlanStep {
    let mut str_params = params;
    let normalized_tool = normalize_test_tool(tool, &mut str_params);
    let parameters: HashMap<String, serde_json::Value> = str_params
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect();
    PlanStep {
        id: id.to_string(),
        task: format!("Test task for {}", normalized_tool),
        tool: Some(normalized_tool),
        parameters,
        expected_outputs: vec![],
        confidence: 1.0,
        metadata: HashMap::new(),
        timeout_override_secs: None,
        ..Default::default()
    }
}

fn normalize_test_tool(tool: &str, params: &mut HashMap<String, String>) -> String {
    if let Some(action) = tool.strip_prefix("browser_") {
        params
            .entry("action".to_string())
            .or_insert_with(|| action.to_string());
        return "browser".to_string();
    }

    if let Some(action) = tool.strip_prefix("file_") {
        let normalized = match action {
            "mkdir" | "create_dir" => "mkdir",
            other => other,
        };
        params
            .entry("action".to_string())
            .or_insert_with(|| normalized.to_string());
        return "files".to_string();
    }

    tool.to_string()
}

fn make_test_plan(steps: Vec<PlanStep>) -> PlanGraph {
    PlanGraph {
        steps,
        edges: vec![],
        unresolved_inputs: vec![],
        confidence: 1.0,
        provenance: PlanProvenance {
            strategy: "integration_test".to_string(),
            generator: Some("test_harness".to_string()),
            notes: Some("Integration test plan".to_string()),
        },
        ..Default::default()
    }
}

fn assert_browser_pack(action: &ExecutableAction) -> &HashMap<String, serde_json::Value> {
    match action {
        ExecutableAction::Pack {
            capability_name,
            resolved_params,
            ..
        } => {
            assert_eq!(capability_name, "browser");
            resolved_params
        },
        other => panic!("expected browser pack action, got {other:?}"),
    }
}

fn assert_browser_pack_action(action: &ExecutableAction, expected_action: &str) {
    let params = assert_browser_pack(action);
    assert_eq!(
        params.get("action").and_then(|value| value.as_str()),
        Some(expected_action)
    );
}

#[test]
fn test_lowering_to_executable_steps_integration() {
    // Test that lowering produces valid executable steps
    let mut params1 = HashMap::new();
    params1.insert("url".to_string(), "https://example.com".to_string());
    let step1 = make_test_step("step-1", "browser_navigate", params1);

    let mut params2 = HashMap::new();
    params2.insert("selector".to_string(), "button.submit".to_string());
    let step2 = make_test_step("step-2", "browser_click", params2);

    let mut params3 = HashMap::new();
    params3.insert("selector".to_string(), ".result".to_string());
    let step3 = make_test_step("step-3", "browser_get_text", params3);

    let plan = make_test_plan(vec![step1, step2, step3]);

    // Test lowering
    let executable_steps = lower_plan_to_executable_steps(&plan).unwrap();

    assert_eq!(executable_steps.len(), 3);
    assert_eq!(executable_steps[0].step_id(), "step-1");
    assert_eq!(executable_steps[1].step_id(), "step-2");
    assert_eq!(executable_steps[2].step_id(), "step-3");

    // Browser steps remain browser pack intents; primitive action selection
    // belongs to the browser inner loop.
    assert_browser_pack_action(executable_steps[0].inner_action(), "navigate");
    assert_browser_pack_action(executable_steps[1].inner_action(), "click");
    assert_browser_pack_action(executable_steps[2].inner_action(), "get_text");
}

#[test]
fn test_execution_context_initialization() {
    // Test that execution context properly initializes with budget tracking
    let context = PlanExecutionContext::with_budget("test-exec", "test-plan", 5.0);

    assert_eq!(context.runtime_execution_id, "test-exec");
    assert_eq!(context.plan_id, "test-plan");
    assert_eq!(context.initial_budget, 5.0);
    assert_eq!(context.remaining_budget, 5.0);
    assert_eq!(context.current_step_idx, 0);
    assert_eq!(context.status, PlanStatus::Pending); // Initially Pending, set to Running when execution starts
    assert!(context.step_results.is_empty());
    assert!(context.budget_spent.is_empty());
}

#[test]
fn test_execution_context_jit_tracking() {
    // Test JIT clarification tracking in execution context
    use magician::magician_v2::strategy::plan::{AskTiming, UnresolvedInput};
    use serde_json::Value;

    let mut context = PlanExecutionContext::new("test-exec", "test-plan");

    // Add unresolved inputs
    let input1 = UnresolvedInput {
        id: "input-1".to_string(),
        parameter: "email".to_string(),
        display_name: "Email Address".to_string(),
        step_id: Some("step-1".to_string()),
        linked_steps: vec!["step-1".to_string()],
        expected_type: Some("string".to_string()),
        json_schema: None,
        prompt: "Please provide your email".to_string(),
        required: true,
        notes: None,
        priority: magician::magician_v2::strategy::plan::QuestionPriority::Critical,
        default_value: None,
        inference_hints: vec![],
        inference_threshold: 0.0,
        auto_fill: None,
        auto_fill_confidence: None,
        ask_timing: AskTiming::JustInTime,
        discovery_timing: magician::magician_v2::strategy::plan::DiscoveryTiming::JustInTime,
        source: magician::magician_v2::strategy::plan::InputSource::Planner,
        created_at: None,
        updated_at: None,
        status: None,
    };

    context.unresolved_inputs.push(input1);

    // Test is_input_resolved
    assert!(!context.is_input_resolved("input-1"));

    // Test record_resolved_input
    context.record_resolved_input(
        "input-1".to_string(),
        Value::String("test@example.com".to_string()),
    );
    assert!(context.is_input_resolved("input-1"));

    // Test get_unresolved_inputs_for_step
    let unresolved = context.get_unresolved_inputs_for_step("step-1");
    assert_eq!(unresolved.len(), 0); // Should be 0 since we resolved it
}

#[test]
fn test_budget_tracking_integration() {
    // Test budget tracking throughout execution context lifecycle
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 10.0);

    // Simulate spending budget
    context.record_budget_spend(2.5, "LLM call for validation", Some("step-1".to_string()));
    assert_eq!(context.remaining_budget, 7.5);
    assert_eq!(context.budget_spent.len(), 1);

    context.record_budget_spend(3.0, "Vision analysis", Some("step-2".to_string()));
    assert_eq!(context.remaining_budget, 4.5);
    assert_eq!(context.budget_spent.len(), 2);

    // Test budget utilization
    assert_eq!(context.budget_utilization(), 0.55); // 5.5/10.0

    // Test budget exhaustion
    assert!(!context.is_budget_exhausted());
    context.record_budget_spend(4.5, "Large operation", Some("step-3".to_string()));
    assert!(context.is_budget_exhausted());
}

#[test]
fn test_multiple_tool_types_in_sequence() {
    // Test that multiple different tool types can be lowered in sequence
    let mut steps = Vec::new();

    // Navigation
    let mut params = HashMap::new();
    params.insert("url".to_string(), "https://example.com".to_string());
    steps.push(make_test_step("step-1", "browser_navigate", params));

    // Wait
    let mut params = HashMap::new();
    params.insert("selector".to_string(), "#content".to_string());
    steps.push(make_test_step(
        "step-2",
        "browser_wait_for_selector",
        params,
    ));

    // Interaction
    let mut params = HashMap::new();
    params.insert("selector".to_string(), "input[name='search']".to_string());
    params.insert("text".to_string(), "test query".to_string());
    steps.push(make_test_step("step-3", "browser_type", params));

    // Screenshot
    let mut params = HashMap::new();
    params.insert("full_page".to_string(), "true".to_string());
    steps.push(make_test_step("step-4", "browser_screenshot", params));

    // Data extraction
    let mut params = HashMap::new();
    params.insert("selector".to_string(), ".results".to_string());
    steps.push(make_test_step("step-5", "browser_get_text", params));

    let plan = make_test_plan(steps);
    let executable_steps = lower_plan_to_executable_steps(&plan).unwrap();

    assert_eq!(executable_steps.len(), 5);

    // Verify all steps were lowered correctly
    for (i, step) in executable_steps.iter().enumerate() {
        assert_eq!(step.step_id(), format!("step-{}", i + 1));
    }
}

#[test]
fn test_error_handling_in_lowering() {
    // Test that lowering properly handles various error cases. Browser steps
    // are intentionally parameter-tolerant because the browser inner loop
    // performs grounding and clarification at runtime.

    // Test 1: Navigate missing URL remains a valid browser intent.
    let params = HashMap::new();
    let step = make_test_step("step-1", "browser_navigate", params);
    let plan = make_test_plan(vec![step]);

    let result = lower_plan_to_executable_steps(&plan);
    assert!(
        result.is_ok(),
        "Browser lowering should preserve incomplete intent for the inner loop"
    );

    // Test 2: Unsupported tool → error
    let params = HashMap::new();
    let step = make_test_step("step-1", "unsupported_tool_xyz", params);
    let plan = make_test_plan(vec![step]);

    let result = lower_plan_to_executable_steps(&plan);
    assert!(result.is_err());

    // Test 3: Click with invalid click_count remains a browser intent.
    let mut params = HashMap::new();
    params.insert("selector".to_string(), "button".to_string());
    params.insert("click_count".to_string(), "invalid_number".to_string());
    let step = make_test_step("step-1", "browser_click", params);
    let plan = make_test_plan(vec![step]);

    let result = lower_plan_to_executable_steps(&plan);
    assert!(
        result.is_ok(),
        "Browser lowering should not parse primitive browser parameters"
    );
}

#[test]
fn test_session_id_extraction() {
    // Test that session IDs are properly extracted from metadata
    let mut params = HashMap::new();
    params.insert("url".to_string(), "https://example.com".to_string());

    let mut step = make_test_step("step-1", "browser_navigate", params);

    // Add session metadata
    step.metadata.insert(
        "session".to_string(),
        r#"{"session_id":"test-session-123"}"#.to_string(),
    );

    let plan = make_test_plan(vec![step]);
    let executable_steps = lower_plan_to_executable_steps(&plan).unwrap();

    assert_eq!(
        executable_steps[0].session_id,
        Some("test-session-123".to_string())
    );
}

#[test]
fn test_timeout_override_propagation() {
    // Test that timeout overrides propagate from PlanStep to ExecutableStep
    let mut params = HashMap::new();
    params.insert("url".to_string(), "https://slow-site.com".to_string());

    let mut step = make_test_step("step-1", "browser_navigate", params);
    step.timeout_override_secs = Some(300); // 5 minutes

    let plan = make_test_plan(vec![step]);
    let executable_steps = lower_plan_to_executable_steps(&plan).unwrap();

    assert_eq!(executable_steps[0].timeout_secs, Some(300));
}

// =============================================================================
// JIT Clarification Integration Tests (Pause/Resume)
// =============================================================================

/// Helper function to create an UnresolvedInput with JIT timing
fn make_jit_input(
    id: &str,
    parameter: &str,
    display_name: &str,
    step_id: &str,
    prompt: &str,
    required: bool,
) -> magician::magician_v2::strategy::plan::UnresolvedInput {
    use magician::magician_v2::strategy::plan::{
        AskTiming, DiscoveryTiming, InputSource, QuestionPriority,
    };

    magician::magician_v2::strategy::plan::UnresolvedInput {
        id: id.to_string(),
        parameter: parameter.to_string(),
        display_name: display_name.to_string(),
        step_id: Some(step_id.to_string()),
        linked_steps: vec![step_id.to_string()],
        expected_type: Some("string".to_string()),
        json_schema: None,
        prompt: prompt.to_string(),
        required,
        notes: None,
        priority: if required {
            QuestionPriority::Critical
        } else {
            QuestionPriority::Optional
        },
        default_value: None,
        inference_hints: vec![],
        inference_threshold: 0.0,
        auto_fill: None,
        auto_fill_confidence: None,
        ask_timing: AskTiming::JustInTime,
        discovery_timing: DiscoveryTiming::JustInTime,
        source: InputSource::Planner,
        created_at: None,
        updated_at: None,
        status: None,
    }
}

#[test]
fn test_jit_pause_resume_single_input() {
    // Test that execution properly tracks state for pause/resume with a single JIT input
    use serde_json::Value;

    let mut context = PlanExecutionContext::new("test-exec", "test-plan");

    // Create a JIT input for step-2
    let jit_input = make_jit_input(
        "input-1",
        "email",
        "Email Address",
        "step-2",
        "Please provide your email",
        true,
    );

    context.unresolved_inputs.push(jit_input);

    // Simulate execution reaching step-2
    context.current_step_idx = 1; // Step-2 index

    // Before resolving: input should be unresolved and needed for step-2
    assert!(!context.is_input_resolved("input-1"));
    let unresolved_for_step2 = context.get_unresolved_inputs_for_step("step-2");
    assert_eq!(unresolved_for_step2.len(), 1);
    assert_eq!(unresolved_for_step2[0].id, "input-1");

    // Simulate pause: check that we can detect unresolved inputs for current step
    let has_unresolved = !context.get_unresolved_inputs_for_step("step-2").is_empty();
    assert!(
        has_unresolved,
        "Should detect unresolved inputs requiring pause"
    );

    // Simulate user providing clarification
    context.record_resolved_input(
        "input-1".to_string(),
        Value::String("user@example.com".to_string()),
    );

    // After resolving: input should be resolved
    assert!(context.is_input_resolved("input-1"));
    let unresolved_after = context.get_unresolved_inputs_for_step("step-2");
    assert_eq!(
        unresolved_after.len(),
        0,
        "Should have no unresolved inputs after clarification"
    );

    // Execution can now resume
    let can_resume = context.get_unresolved_inputs_for_step("step-2").is_empty();
    assert!(can_resume, "Should be able to resume execution");
}

#[test]
fn test_jit_multiple_inputs_same_step() {
    // Test pause/resume with multiple JIT inputs for the same step
    use serde_json::Value;

    let mut context = PlanExecutionContext::new("test-exec", "test-plan");

    // Create multiple JIT inputs for step-3
    let input1 = make_jit_input(
        "input-1",
        "username",
        "Username",
        "step-3",
        "Enter username",
        true,
    );
    let input2 = make_jit_input(
        "input-2",
        "password",
        "Password",
        "step-3",
        "Enter password",
        true,
    );

    context.unresolved_inputs.push(input1);
    context.unresolved_inputs.push(input2);

    // Simulate execution reaching step-3
    context.current_step_idx = 2;

    // Should have 2 unresolved inputs
    let unresolved = context.get_unresolved_inputs_for_step("step-3");
    assert_eq!(unresolved.len(), 2);

    // Resolve first input
    context.record_resolved_input("input-1".to_string(), Value::String("admin".to_string()));

    // Should still have 1 unresolved
    let unresolved_after_first = context.get_unresolved_inputs_for_step("step-3");
    assert_eq!(
        unresolved_after_first.len(),
        1,
        "Should still have one unresolved input"
    );

    // Cannot resume yet
    let can_resume = context.get_unresolved_inputs_for_step("step-3").is_empty();
    assert!(
        !can_resume,
        "Should not be able to resume with pending inputs"
    );

    // Resolve second input
    context.record_resolved_input(
        "input-2".to_string(),
        Value::String("secret123".to_string()),
    );

    // Now all inputs resolved
    let unresolved_final = context.get_unresolved_inputs_for_step("step-3");
    assert_eq!(unresolved_final.len(), 0);

    // Can resume
    let can_resume_now = context.get_unresolved_inputs_for_step("step-3").is_empty();
    assert!(
        can_resume_now,
        "Should be able to resume after all inputs resolved"
    );
}

#[test]
fn test_jit_input_linked_to_multiple_steps() {
    // Test JIT input that's shared across multiple steps
    use magician::magician_v2::strategy::plan::{
        AskTiming, DiscoveryTiming, InputSource, QuestionPriority,
    };
    use serde_json::Value;

    let mut context = PlanExecutionContext::new("test-exec", "test-plan");

    // Create a JIT input linked to multiple steps
    let shared_input = magician::magician_v2::strategy::plan::UnresolvedInput {
        id: "input-shared".to_string(),
        parameter: "api_key".to_string(),
        display_name: "API Key".to_string(),
        step_id: Some("step-2".to_string()), // Primary step
        linked_steps: vec![
            "step-2".to_string(),
            "step-4".to_string(),
            "step-6".to_string(),
        ],
        expected_type: Some("string".to_string()),
        json_schema: None,
        prompt: "Enter API key".to_string(),
        required: true,
        notes: None,
        priority: QuestionPriority::Critical,
        default_value: None,
        inference_hints: vec![],
        inference_threshold: 0.0,
        auto_fill: None,
        auto_fill_confidence: None,
        ask_timing: AskTiming::JustInTime,
        discovery_timing: DiscoveryTiming::JustInTime,
        source: InputSource::Planner,
        created_at: None,
        updated_at: None,
        status: None,
    };

    context.unresolved_inputs.push(shared_input);

    // Input should be needed for all linked steps
    assert!(!context.get_unresolved_inputs_for_step("step-2").is_empty());
    assert!(!context.get_unresolved_inputs_for_step("step-4").is_empty());
    assert!(!context.get_unresolved_inputs_for_step("step-6").is_empty());

    // Resolve once
    context.record_resolved_input(
        "input-shared".to_string(),
        Value::String("sk-abc123".to_string()),
    );

    // Should be resolved for all linked steps
    assert!(context.get_unresolved_inputs_for_step("step-2").is_empty());
    assert!(context.get_unresolved_inputs_for_step("step-4").is_empty());
    assert!(context.get_unresolved_inputs_for_step("step-6").is_empty());
}

#[test]
fn test_jit_execution_without_unresolved_inputs() {
    // Baseline: execution flow when no JIT inputs are needed
    let mut context = PlanExecutionContext::new("test-exec", "test-plan");

    // No unresolved inputs added

    // Simulate stepping through execution
    for step_idx in 0..5 {
        context.current_step_idx = step_idx;
        let step_id = format!("step-{}", step_idx + 1);

        // Should have no unresolved inputs for any step
        let unresolved = context.get_unresolved_inputs_for_step(&step_id);
        assert_eq!(
            unresolved.len(),
            0,
            "Step {} should have no unresolved inputs",
            step_id
        );

        // Execution should never need to pause
        let needs_pause = !context.get_unresolved_inputs_for_step(&step_id).is_empty();
        assert!(!needs_pause, "Step {} should not require pause", step_id);
    }
}

#[test]
fn test_jit_resolution_order_during_execution() {
    // Test that JIT inputs are encountered in the correct order during execution
    use serde_json::Value;

    let mut context = PlanExecutionContext::new("test-exec", "test-plan");

    // Create JIT inputs for different steps
    let input_step2 = make_jit_input(
        "input-step2",
        "param2",
        "Param 2",
        "step-2",
        "Enter param for step 2",
        true,
    );
    let input_step4 = make_jit_input(
        "input-step4",
        "param4",
        "Param 4",
        "step-4",
        "Enter param for step 4",
        true,
    );
    let input_step1 = make_jit_input(
        "input-step1",
        "param1",
        "Param 1",
        "step-1",
        "Enter param for step 1",
        true,
    );

    context.unresolved_inputs.push(input_step2);
    context.unresolved_inputs.push(input_step4);
    context.unresolved_inputs.push(input_step1);

    // Simulate execution sequence
    // Step 1: Should encounter input-step1
    context.current_step_idx = 0;
    let step1_inputs = context.get_unresolved_inputs_for_step("step-1");
    assert_eq!(step1_inputs.len(), 1);
    assert_eq!(step1_inputs[0].id, "input-step1");
    context.record_resolved_input("input-step1".to_string(), Value::String("val1".to_string()));

    // Step 2: Should encounter input-step2
    context.current_step_idx = 1;
    let step2_inputs = context.get_unresolved_inputs_for_step("step-2");
    assert_eq!(step2_inputs.len(), 1);
    assert_eq!(step2_inputs[0].id, "input-step2");
    context.record_resolved_input("input-step2".to_string(), Value::String("val2".to_string()));

    // Step 3: No inputs
    context.current_step_idx = 2;
    let step3_inputs = context.get_unresolved_inputs_for_step("step-3");
    assert_eq!(step3_inputs.len(), 0);

    // Step 4: Should encounter input-step4
    context.current_step_idx = 3;
    let step4_inputs = context.get_unresolved_inputs_for_step("step-4");
    assert_eq!(step4_inputs.len(), 1);
    assert_eq!(step4_inputs[0].id, "input-step4");
    context.record_resolved_input("input-step4".to_string(), Value::String("val4".to_string()));

    // Verify all inputs resolved
    assert!(context.is_input_resolved("input-step1"));
    assert!(context.is_input_resolved("input-step2"));
    assert!(context.is_input_resolved("input-step4"));
}

#[test]
fn test_jit_mixed_required_and_optional_inputs() {
    // Test handling of both required and optional JIT inputs
    use magician::magician_v2::strategy::plan::{
        AskTiming, DiscoveryTiming, InputSource, QuestionPriority,
    };
    use serde_json::Value;

    let mut context = PlanExecutionContext::new("test-exec", "test-plan");

    // Required input
    let required_input = make_jit_input(
        "input-required",
        "username",
        "Username",
        "step-1",
        "Enter username (required)",
        true,
    );

    // Optional input
    let optional_input = magician::magician_v2::strategy::plan::UnresolvedInput {
        id: "input-optional".to_string(),
        parameter: "nickname".to_string(),
        display_name: "Nickname".to_string(),
        step_id: Some("step-1".to_string()),
        linked_steps: vec!["step-1".to_string()],
        expected_type: Some("string".to_string()),
        json_schema: None,
        prompt: "Enter nickname (optional)".to_string(),
        required: false,
        notes: None,
        priority: QuestionPriority::Optional,
        default_value: None,
        inference_hints: vec![],
        inference_threshold: 0.0,
        auto_fill: None,
        auto_fill_confidence: None,
        ask_timing: AskTiming::JustInTime,
        discovery_timing: DiscoveryTiming::JustInTime,
        source: InputSource::Planner,
        created_at: None,
        updated_at: None,
        status: None,
    };

    context.unresolved_inputs.push(required_input);
    context.unresolved_inputs.push(optional_input);

    // Both inputs should be detected
    let unresolved = context.get_unresolved_inputs_for_step("step-1");
    assert_eq!(unresolved.len(), 2);

    // Verify required vs optional
    let required_count = unresolved.iter().filter(|i| i.required).count();
    let optional_count = unresolved.iter().filter(|i| !i.required).count();
    assert_eq!(required_count, 1);
    assert_eq!(optional_count, 1);

    // Resolve only required input
    context.record_resolved_input(
        "input-required".to_string(),
        Value::String("admin".to_string()),
    );

    // Optional still unresolved, but execution could proceed
    // (In practice, executor would handle optional inputs differently)
    let remaining = context.get_unresolved_inputs_for_step("step-1");
    assert_eq!(remaining.len(), 1);
    assert!(!remaining[0].required);
}

#[test]
fn test_jit_state_preservation_across_steps() {
    // Test that resolved inputs are preserved as execution progresses through steps
    use serde_json::Value;

    let mut context = PlanExecutionContext::new("test-exec", "test-plan");

    // Add inputs for different steps
    let input1 = make_jit_input("input-1", "p1", "P1", "step-1", "Prompt 1", true);
    let input2 = make_jit_input("input-2", "p2", "P2", "step-3", "Prompt 2", true);

    context.unresolved_inputs.push(input1);
    context.unresolved_inputs.push(input2);

    // Step 1: Resolve input-1
    context.current_step_idx = 0;
    context.record_resolved_input("input-1".to_string(), Value::String("value1".to_string()));
    assert!(context.is_input_resolved("input-1"));

    // Step 2: No inputs, but input-1 should still be resolved
    context.current_step_idx = 1;
    assert!(
        context.is_input_resolved("input-1"),
        "Previously resolved input should remain resolved"
    );
    assert!(
        !context.is_input_resolved("input-2"),
        "Future input should not be resolved yet"
    );

    // Step 3: Resolve input-2
    context.current_step_idx = 2;
    context.record_resolved_input("input-2".to_string(), Value::String("value2".to_string()));
    assert!(context.is_input_resolved("input-2"));

    // Step 4: Both should still be resolved
    context.current_step_idx = 3;
    assert!(
        context.is_input_resolved("input-1"),
        "Earlier resolved input should persist"
    );
    assert!(
        context.is_input_resolved("input-2"),
        "Recently resolved input should persist"
    );

    // Verify resolved values are accessible
    assert_eq!(context.resolved_input_values.len(), 2);
}

#[test]
fn test_jit_budget_tracking_during_pause_resume() {
    // Test that budget tracking continues correctly across pause/resume cycles
    use serde_json::Value;

    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 100.0);

    // Add JIT input for step-2
    let input = make_jit_input(
        "input-1",
        "param",
        "Parameter",
        "step-2",
        "Enter parameter",
        true,
    );
    context.unresolved_inputs.push(input);

    // Execute step-1, spend budget
    context.current_step_idx = 0;
    context.record_budget_spend(10.0, "Step 1 execution", Some("step-1".to_string()));
    assert_eq!(context.remaining_budget, 90.0);

    // Reach step-2, need to pause for input
    context.current_step_idx = 1;
    let needs_pause = !context.get_unresolved_inputs_for_step("step-2").is_empty();
    assert!(needs_pause);

    // Spend budget during pause (e.g., for asking user)
    context.record_budget_spend(5.0, "JIT clarification prompt", Some("step-2".to_string()));
    assert_eq!(context.remaining_budget, 85.0);

    // Resolve input
    context.record_resolved_input("input-1".to_string(), Value::String("value".to_string()));

    // Resume execution, spend more budget
    context.record_budget_spend(15.0, "Step 2 execution", Some("step-2".to_string()));
    assert_eq!(context.remaining_budget, 70.0);

    // Continue to step-3
    context.current_step_idx = 2;
    context.record_budget_spend(20.0, "Step 3 execution", Some("step-3".to_string()));
    assert_eq!(context.remaining_budget, 50.0);

    // Verify total budget spent
    assert_eq!(context.budget_spent.len(), 4);
    assert_eq!(context.budget_utilization(), 0.5); // 50/100
}

#[test]
fn test_jit_plan_with_mixed_upfront_and_jit_inputs() {
    // Test execution with both upfront-resolved and JIT inputs
    use magician::magician_v2::strategy::plan::{
        AskTiming, DiscoveryTiming, InputSource, QuestionPriority,
    };
    use serde_json::Value;

    let mut context = PlanExecutionContext::new("test-exec", "test-plan");

    // Upfront input (already resolved before execution starts)
    let upfront_input = magician::magician_v2::strategy::plan::UnresolvedInput {
        id: "input-upfront".to_string(),
        parameter: "base_url".to_string(),
        display_name: "Base URL".to_string(),
        step_id: Some("step-1".to_string()),
        linked_steps: vec!["step-1".to_string()],
        expected_type: Some("string".to_string()),
        json_schema: None,
        prompt: "Enter base URL".to_string(),
        required: true,
        notes: None,
        priority: QuestionPriority::Critical,
        default_value: None,
        inference_hints: vec![],
        inference_threshold: 0.0,
        auto_fill: None,
        auto_fill_confidence: None,
        ask_timing: AskTiming::PreExecution,
        discovery_timing: DiscoveryTiming::PreExecution,
        source: InputSource::Planner,
        created_at: None,
        updated_at: None,
        status: None,
    };

    // JIT input (resolved during execution)
    let jit_input = make_jit_input(
        "input-jit",
        "session_id",
        "Session ID",
        "step-3",
        "Enter session ID",
        true,
    );

    context.unresolved_inputs.push(upfront_input);
    context.unresolved_inputs.push(jit_input);

    // Pre-resolve upfront input (happens before execution starts)
    context.record_resolved_input(
        "input-upfront".to_string(),
        Value::String("https://api.example.com".to_string()),
    );

    // Start execution - step 1
    context.current_step_idx = 0;
    let step1_unresolved = context.get_unresolved_inputs_for_step("step-1");
    assert_eq!(
        step1_unresolved.len(),
        0,
        "Upfront input should already be resolved"
    );

    // Step 2 - no inputs
    context.current_step_idx = 1;
    assert_eq!(context.get_unresolved_inputs_for_step("step-2").len(), 0);

    // Step 3 - encounter JIT input
    context.current_step_idx = 2;
    let step3_unresolved = context.get_unresolved_inputs_for_step("step-3");
    assert_eq!(step3_unresolved.len(), 1, "Should need JIT input");
    assert_eq!(step3_unresolved[0].ask_timing, AskTiming::JustInTime);

    // Resolve JIT input
    context.record_resolved_input(
        "input-jit".to_string(),
        Value::String("sess-xyz789".to_string()),
    );

    // Verify both inputs resolved
    assert!(context.is_input_resolved("input-upfront"));
    assert!(context.is_input_resolved("input-jit"));
    assert_eq!(context.resolved_input_values.len(), 2);
}

// =============================================================================
// Budget Tracking Edge Case Tests
// =============================================================================

#[test]
fn test_budget_zero_initial_allocation() {
    // Test execution context with zero initial budget
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 0.0);

    assert_eq!(context.initial_budget, 0.0);
    assert_eq!(context.remaining_budget, 0.0);
    assert!(context.is_budget_exhausted());

    // Spend is tracked, but remaining budget is clamped to 0.0 minimum
    context.record_budget_spend(1.0, "Operation", Some("step-1".to_string()));
    assert_eq!(context.remaining_budget, 0.0); // Clamped to minimum 0.0
    assert!(context.is_budget_exhausted());
    assert_eq!(context.budget_spent.len(), 1); // Spend is still recorded
}

#[test]
fn test_budget_negative_spend_amount() {
    // Test that negative spend amounts are ignored (implementation detail)
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 100.0);

    // Normal spend
    context.record_budget_spend(30.0, "Operation", Some("step-1".to_string()));
    assert_eq!(context.remaining_budget, 70.0);

    // Negative spend is ignored (amounts <= 0.0 are rejected)
    context.record_budget_spend(-10.0, "Budget correction", Some("step-1".to_string()));
    assert_eq!(context.remaining_budget, 70.0); // No change - negative amount ignored

    // Only the positive spend is recorded
    assert_eq!(context.budget_spent.len(), 1);
}

#[test]
fn test_budget_spend_without_step_id() {
    // Test budget spend without associating with a specific step
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 50.0);

    // Spend without step_id (global overhead)
    context.record_budget_spend(5.0, "Global overhead", None);
    assert_eq!(context.remaining_budget, 45.0);
    assert_eq!(context.budget_spent.len(), 1);
    assert_eq!(context.budget_spent[0].step_id, None);

    // Mixed: some with step_id, some without
    context.record_budget_spend(10.0, "Step operation", Some("step-1".to_string()));
    context.record_budget_spend(3.0, "Another overhead", None);

    assert_eq!(context.remaining_budget, 32.0);
    assert_eq!(context.budget_spent.len(), 3);
}

#[test]
fn test_budget_multiple_spends_same_step() {
    // Test multiple budget spends on the same step
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 100.0);

    // Multiple operations for step-2
    context.record_budget_spend(10.0, "Vision analysis", Some("step-2".to_string()));
    context.record_budget_spend(5.0, "DOM analysis", Some("step-2".to_string()));
    context.record_budget_spend(8.0, "Action validation", Some("step-2".to_string()));

    assert_eq!(context.remaining_budget, 77.0);
    assert_eq!(context.budget_spent.len(), 3);

    // Verify all spends recorded
    let step2_spends: Vec<_> = context
        .budget_spent
        .iter()
        .filter(|s| s.step_id.as_deref() == Some("step-2"))
        .collect();
    assert_eq!(step2_spends.len(), 3);
}

#[test]
fn test_budget_utilization_boundary_conditions() {
    // Test budget utilization at 0%, 50%, 100%, and >100%
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 100.0);

    // 0% utilization
    assert_eq!(context.budget_utilization(), 0.0);
    assert!(!context.is_budget_exhausted());

    // 50% utilization
    context.record_budget_spend(50.0, "Half budget", Some("step-1".to_string()));
    assert_eq!(context.budget_utilization(), 0.5);
    assert!(!context.is_budget_exhausted());

    // 100% utilization
    context.record_budget_spend(50.0, "Rest of budget", Some("step-2".to_string()));
    assert_eq!(context.budget_utilization(), 1.0);
    assert!(context.is_budget_exhausted());

    // >100% utilization (overspend) - remaining budget clamped to 0.0
    // Since remaining_budget is clamped, utilization maxes out at 1.0
    context.record_budget_spend(25.0, "Overspend", Some("step-3".to_string()));
    assert_eq!(context.budget_utilization(), 1.0); // Maxes at 1.0 due to clamping
    assert!(context.is_budget_exhausted());
    assert_eq!(context.remaining_budget, 0.0); // Clamped to 0.0
}

#[test]
fn test_budget_overflow_scenario() {
    // Test budget overflow (spending significantly more than allocated)
    // Implementation clamps remaining_budget to minimum 0.0, so utilization maxes at 1.0
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 10.0);

    // Spend way more than budget
    context.record_budget_spend(100.0, "Large operation", Some("step-1".to_string()));

    assert_eq!(context.remaining_budget, 0.0); // Clamped to 0.0
    assert!(context.is_budget_exhausted());
    assert_eq!(context.budget_utilization(), 1.0); // Maxes at 1.0 due to clamping

    // Can still track more spends even when over budget
    context.record_budget_spend(50.0, "Another operation", Some("step-2".to_string()));
    assert_eq!(context.remaining_budget, 0.0); // Still clamped to 0.0
    assert_eq!(context.budget_utilization(), 1.0); // Still maxes at 1.0

    // But budget_spent records track actual spending
    assert_eq!(context.budget_spent.len(), 2);
    let total_spent: f64 = context.budget_spent.iter().map(|s| s.amount).sum();
    assert_eq!(total_spent, 150.0); // Actual total spent is tracked
}

#[test]
fn test_budget_exhaustion_boundary() {
    // Test exact budget exhaustion vs slightly over/under
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 100.0);

    // Just under budget (99.99)
    context.record_budget_spend(99.99, "Almost all", Some("step-1".to_string()));
    assert!(!context.is_budget_exhausted());
    // Use approximate comparison for floating point
    let diff = (context.remaining_budget - 0.01).abs();
    assert!(
        diff < 1e-10,
        "Expected ~0.01, got {}",
        context.remaining_budget
    );

    // Reset for exact test
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 100.0);

    // Exactly at budget
    context.record_budget_spend(100.0, "Exact budget", Some("step-1".to_string()));
    assert!(context.is_budget_exhausted());
    assert_eq!(context.remaining_budget, 0.0);

    // Reset for over test
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 100.0);

    // Just over budget (100.01) - remaining budget clamped to 0.0
    context.record_budget_spend(100.01, "Slightly over", Some("step-1".to_string()));
    assert!(context.is_budget_exhausted());
    assert_eq!(context.remaining_budget, 0.0); // Clamped to 0.0
}

#[test]
fn test_budget_with_empty_reason() {
    // Test budget spend with empty reason string
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 50.0);

    context.record_budget_spend(10.0, "", Some("step-1".to_string()));
    assert_eq!(context.remaining_budget, 40.0);
    assert_eq!(context.budget_spent.len(), 1);
    assert_eq!(context.budget_spent[0].reason, "");
}

#[test]
fn test_budget_large_number_of_spends() {
    // Test tracking large number of small budget spends
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 1000.0);

    // 100 small spends
    for i in 0..100 {
        let step_id = format!("step-{}", i % 10); // Distribute across 10 steps
        context.record_budget_spend(1.0, format!("Spend {}", i), Some(step_id));
    }

    assert_eq!(context.budget_spent.len(), 100);
    assert_eq!(context.remaining_budget, 900.0);
    assert_eq!(context.budget_utilization(), 0.1);
}

#[test]
fn test_budget_floating_point_precision() {
    // Test budget tracking with floating point precision edge cases
    let mut context = PlanExecutionContext::with_budget("test-exec", "test-plan", 10.0);

    // Multiple small float operations that could accumulate rounding errors
    context.record_budget_spend(0.1, "Spend 1", Some("step-1".to_string()));
    context.record_budget_spend(0.2, "Spend 2", Some("step-1".to_string()));
    context.record_budget_spend(0.3, "Spend 3", Some("step-1".to_string()));
    context.record_budget_spend(0.4, "Spend 4", Some("step-1".to_string()));

    // 0.1 + 0.2 + 0.3 + 0.4 = 1.0
    let expected_remaining = 10.0 - 1.0;
    let diff = (context.remaining_budget - expected_remaining).abs();
    assert!(
        diff < 1e-10,
        "Floating point precision issue: diff = {}",
        diff
    );
}

#[test]
fn test_budget_context_initialization_variants() {
    // Test different ways to initialize budget in context

    // Default budget is $1.00
    let ctx1 = PlanExecutionContext::new("exec-1", "plan-1");
    assert_eq!(ctx1.initial_budget, 1.0);
    assert_eq!(ctx1.remaining_budget, 1.0);

    // With explicit budget
    let ctx2 = PlanExecutionContext::with_budget("exec-2", "plan-2", 50.0);
    assert_eq!(ctx2.initial_budget, 50.0);
    assert_eq!(ctx2.remaining_budget, 50.0);

    // Large budget
    let ctx3 = PlanExecutionContext::with_budget("exec-3", "plan-3", 1_000_000.0);
    assert_eq!(ctx3.initial_budget, 1_000_000.0);
    assert_eq!(ctx3.remaining_budget, 1_000_000.0);
}

// ============================================================================
// Selector Validation Integration Tests
// ============================================================================

#[test]
fn test_browser_lowering_preserves_unresolved_selector_intent() {
    // Browser lowering preserves unresolved selector intent. The browser inner
    // loop resolves the right primitive action at runtime from page state.

    // Step 1: Navigate (has all required params).
    let mut params1 = HashMap::new();
    params1.insert("url".to_string(), "https://example.com".to_string());
    let step1 = make_test_step("step-1", "browser_navigate", params1);

    // Step 2: Click WITH selector.
    let mut params2 = HashMap::new();
    params2.insert("selector".to_string(), "#login-btn".to_string());
    let step2 = make_test_step("step-2", "browser_click", params2);

    // Step 3: Click WITHOUT selector.
    let step3 = make_test_step("step-3", "browser_click", HashMap::new());

    // Step 4: Type WITHOUT selector.
    let mut params4 = HashMap::new();
    params4.insert("text".to_string(), "hello@world.com".to_string());
    let step4 = make_test_step("step-4", "browser_type", params4);

    let plan = make_test_plan(vec![step1, step2, step3, step4]);

    let steps = lower_plan_to_executable_steps(&plan)
        .expect("Browser intent lowering should preserve all steps");
    assert_eq!(steps.len(), 4);

    let step2_params = assert_browser_pack(steps[1].inner_action());
    assert_eq!(
        step2_params
            .get("selector")
            .and_then(|value| value.as_str()),
        Some("#login-btn")
    );

    let step3_params = assert_browser_pack(steps[2].inner_action());
    assert_eq!(
        step3_params.get("action").and_then(|value| value.as_str()),
        Some("click")
    );
    assert!(step3_params.get("selector").is_none());

    let step4_params = assert_browser_pack(steps[3].inner_action());
    assert_eq!(
        step4_params.get("action").and_then(|value| value.as_str()),
        Some("type")
    );
    assert_eq!(
        step4_params.get("text").and_then(|value| value.as_str()),
        Some("hello@world.com")
    );
    assert!(step4_params.get("selector").is_none());
}

#[test]
fn test_browser_ref_parameter_stays_in_pack() {
    // Browser lowering preserves `ref`; the browser inner loop decides how to
    // use it against the current page state.
    let mut params = HashMap::new();
    params.insert("ref".to_string(), "#my-element".to_string());

    let step = make_test_step("step-1", "browser_click", params);
    let plan = make_test_plan(vec![step]);

    let executable_steps = lower_plan_to_executable_steps(&plan).unwrap();
    let params = assert_browser_pack(executable_steps[0].inner_action());
    assert_eq!(
        params.get("ref").and_then(|value| value.as_str()),
        Some("#my-element")
    );
}

#[test]
fn test_drag_and_drop_missing_selectors_stays_browser_pack() {
    // The browser inner loop resolves source/target at runtime.
    let step = make_test_step("step-1", "browser_drag_and_drop", HashMap::new());

    let plan = make_test_plan(vec![step]);

    let steps =
        lower_plan_to_executable_steps(&plan).expect("Browser intent lowering should succeed");
    assert_eq!(steps.len(), 1);
    assert_browser_pack_action(steps[0].inner_action(), "drag_and_drop");
}

#[test]
fn test_missing_selector_stays_browser_pack_for_runtime_resolution() {
    // End-to-end check: the browser inner loop resolves the element at runtime.

    let step = make_test_step("step-to-resolve", "browser_click", HashMap::new());
    let plan = make_test_plan(vec![step]);

    let steps =
        lower_plan_to_executable_steps(&plan).expect("Browser intent lowering should succeed");
    assert_eq!(steps.len(), 1);
    assert_browser_pack_action(steps[0].inner_action(), "click");
}

#[test]
fn test_drag_and_drop_missing_selectors_stays_browser_pack_e2e() {
    // End-to-end variant: drag-and-drop remains an intent for the browser inner loop.

    let step = make_test_step("drag-step", "browser_drag_and_drop", HashMap::new());
    let plan = make_test_plan(vec![step]);

    let steps =
        lower_plan_to_executable_steps(&plan).expect("Browser intent lowering should succeed");
    assert_eq!(steps.len(), 1);
    assert_browser_pack_action(steps[0].inner_action(), "drag_and_drop");
}

// =============================================================================
// Native Execution Path Tests (File/HTTP/Shell)
// =============================================================================
//
// These tests verify the complete JIT lowering → native execution → result flow
// for non-browser actions.

use std::path::PathBuf;

#[test]
fn test_native_file_action_jit_lowering_produces_correct_type() {
    // Create a file_write step
    let mut params = HashMap::new();
    params.insert("path".to_string(), "/tmp/test_output.txt".to_string());
    params.insert("content".to_string(), "Hello, World!".to_string());

    let step = make_test_step("step-file-write", "file_write", params);

    // Verify tool type detection
    assert!(is_file_tool("files"));
    assert!(!is_browser_tool("files"));
    assert!(!is_http_tool("files"));
    assert!(!is_shell_tool("files"));

    // JIT lower the step
    let action =
        lower_step_to_executable_action(&step).expect("JIT lowering should succeed for file_write");

    // Verify it's a FileAction
    assert!(action.is_file(), "Should produce FileAction");

    if let ExecutableAction::File(FileAction::Write {
        path,
        content,
        create_dirs,
    }) = action
    {
        assert_eq!(path, PathBuf::from("/tmp/test_output.txt"));
        assert_eq!(content, "Hello, World!");
        assert!(create_dirs, "create_dirs should default to true");
    } else {
        panic!("Expected FileAction::Write");
    }
}

#[test]
fn test_native_http_action_jit_lowering_produces_correct_type() {
    // Create an http_get step
    let mut params: HashMap<String, serde_json::Value> = HashMap::new();
    params.insert(
        "url".to_string(),
        serde_json::Value::String("https://api.example.com/data".to_string()),
    );

    let step = PlanStep {
        id: "step-http-get".to_string(),
        task: "Fetch API data".to_string(),
        tool: Some("http_get".to_string()),
        parameters: params,
        expected_outputs: vec![],
        confidence: 1.0,
        metadata: HashMap::new(),
        timeout_override_secs: None,
        ..Default::default()
    };

    // Verify tool type detection
    assert!(is_http_tool("http_get"));
    assert!(!is_file_tool("http_get"));
    assert!(!is_shell_tool("http_get"));

    // JIT lower the step
    let action =
        lower_step_to_executable_action(&step).expect("JIT lowering should succeed for http_get");

    // Verify it's an HttpAction
    assert!(action.is_http(), "Should produce HttpAction");

    if let ExecutableAction::Http(http_action) = action {
        assert_eq!(http_action.url, "https://api.example.com/data");
        assert!(matches!(
            http_action.method,
            magician::magician_v2::execution::HttpMethod::Get
        ));
    } else {
        panic!("Expected HttpAction");
    }
}

#[test]
fn test_native_shell_action_jit_lowering_produces_correct_type() {
    // Create a shell step
    let mut params = HashMap::new();
    params.insert("command".to_string(), "echo 'Hello from bash'".to_string());

    let step = make_test_step("step-bash", "shell", params);

    // Verify tool type detection
    assert!(is_shell_tool("shell"));
    assert!(!is_file_tool("shell"));
    assert!(!is_http_tool("shell"));

    // JIT lower the step
    let action =
        lower_step_to_executable_action(&step).expect("JIT lowering should succeed for shell");

    // Verify it's a BashAction
    assert!(action.is_bash(), "Should produce BashAction");

    if let ExecutableAction::Bash(bash_action) = action {
        assert_eq!(bash_action.command, "echo 'Hello from bash'");
        assert!(
            bash_action.capture_output,
            "capture_output should default to true"
        );
    } else {
        panic!("Expected BashAction");
    }
}

#[tokio::test]
async fn test_native_file_action_execution_e2e() {
    // End-to-end test: JIT lower -> execute -> verify result

    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let test_file = temp_dir.path().join("e2e_test.txt");
    let test_content = "Native execution e2e test content";

    // Create file write step
    let mut params = HashMap::new();
    params.insert("path".to_string(), test_file.to_string_lossy().to_string());
    params.insert("content".to_string(), test_content.to_string());

    let step = make_test_step("step-e2e-file", "file_write", params);

    // Step 1: JIT lower to ExecutableAction
    let action = lower_step_to_executable_action(&step).expect("JIT lowering should succeed");

    // Step 2: Execute the file action
    if let ExecutableAction::File(file_action) = &action {
        let result = execute_file_action(file_action, &unrestricted_file_sandbox())
            .await
            .expect("File action execution should succeed");

        // Step 3: Verify the result
        assert!(result.is_success(), "File write should succeed");

        // Step 4: Verify the file was actually written
        let file_contents =
            std::fs::read_to_string(&test_file).expect("Should be able to read the test file");
        assert_eq!(file_contents, test_content);
    } else {
        panic!("Expected FileAction from JIT lowering");
    }
}

#[tokio::test]
async fn test_native_shell_action_execution_e2e() {
    // End-to-end test: JIT lower -> execute -> verify result

    // Create bash step that produces output
    let mut params = HashMap::new();
    params.insert(
        "command".to_string(),
        "echo 'e2e_bash_test_output'".to_string(),
    );

    let step = make_test_step("step-e2e-bash", "shell", params);

    // Step 1: JIT lower to ExecutableAction
    let action = lower_step_to_executable_action(&step).expect("JIT lowering should succeed");

    // Step 2: Execute the bash action
    if let ExecutableAction::Bash(bash_action) = &action {
        let result = execute_bash_action(bash_action, &ShellSandboxConfig::default(), None, None)
            .await
            .expect("Bash action execution should succeed");

        // Step 3: Verify the result contains expected output
        let output_text = result.as_text().expect("Bash should return text output");
        assert!(
            output_text.contains("e2e_bash_test_output"),
            "Output should contain the echo'd text: {}",
            output_text
        );
    } else {
        panic!("Expected BashAction from JIT lowering");
    }
}

#[test]
fn test_plan_lowering_with_mixed_browser_and_native_steps() {
    // Test that a plan with both browser and native steps lowers successfully

    // Browser step
    let mut browser_params = HashMap::new();
    browser_params.insert("url".to_string(), "https://example.com".to_string());
    let browser_step = make_test_step("step-browser", "browser_navigate", browser_params);

    // File step
    let mut file_params = HashMap::new();
    file_params.insert("path".to_string(), "/tmp/output.txt".to_string());
    file_params.insert("content".to_string(), "result data".to_string());
    let file_step = make_test_step("step-file", "file_write", file_params);

    // Bash step
    let mut bash_params = HashMap::new();
    bash_params.insert("command".to_string(), "echo done".to_string());
    let bash_step = make_test_step("step-bash", "shell", bash_params);

    let plan = make_test_plan(vec![browser_step, file_step, bash_step]);

    // Lower the entire plan
    let executable_steps =
        lower_plan_to_executable_steps(&plan).expect("Mixed plan lowering should succeed");

    assert_eq!(executable_steps.len(), 3, "Should have 3 executable steps");

    assert_browser_pack_action(executable_steps[0].inner_action(), "navigate");

    // File and Bash steps now have real ExecutableAction types directly (no more placeholders!)
    assert!(
        executable_steps[1].action.is_file(),
        "File step should have FileAction"
    );
    assert!(
        executable_steps[2].action.is_bash(),
        "Bash step should have BashAction"
    );
}

use magician::magician_v2::execution::is_browser_tool;

// === Native Action Direct Execution Tests ===
// These tests verify that native actions are executed correctly through the direct execution path.
// Native actions (file, bash, http) bypass the agentic loop and are executed directly.

use magician::magician_v2::execution::ActionResult;

/// Test that a file action executes correctly through direct execution path.
/// Native actions bypass the agentic loop and execute directly.
#[tokio::test]
async fn test_observe_act_loop_native_file_action_through_execute_step_as_goal() {
    // Step 1: Create a temp file path for the test
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let test_file = temp_dir.path().join("direct_exec_test.txt");
    let content = "Content written through direct execution";

    // Step 2: Create a file_write step
    let mut params = HashMap::new();
    params.insert("path".to_string(), test_file.to_string_lossy().to_string());
    params.insert("content".to_string(), content.to_string());
    let plan_step = make_test_step("step-direct-file", "file_write", params);

    // Step 3: Lower the plan step to an executable step
    let plan = make_test_plan(vec![plan_step]);
    let executable_steps =
        lower_plan_to_executable_steps(&plan).expect("Plan lowering should succeed");
    assert_eq!(executable_steps.len(), 1);
    let step = &executable_steps[0];

    // Verify the step has a real FileAction
    assert!(
        step.action.is_file(),
        "Should have real FileAction for file action step"
    );

    let result = match step.inner_action() {
        ExecutableAction::File(action) => execute_file_action(action, &unrestricted_file_sandbox())
            .await
            .expect("Direct execution should succeed for file action"),
        other => panic!("Expected file action, got {:?}", other),
    };

    assert!(
        matches!(result, ActionResult::Success),
        "File action should succeed"
    );

    // Verify the file was actually created
    assert!(
        test_file.exists(),
        "File should have been created by the native executor"
    );
    let written_content = std::fs::read_to_string(&test_file).expect("Should read file");
    assert_eq!(written_content, content, "File content should match");
}

/// Test that a shell action executes correctly through direct execution path.
#[tokio::test]
async fn test_observe_act_loop_native_shell_action_through_execute_step_as_goal() {
    // Step 1: Create a shell step
    let mut params = HashMap::new();
    params.insert(
        "command".to_string(),
        "echo 'direct_exec_bash_test'".to_string(),
    );
    let plan_step = make_test_step("step-direct-bash", "shell", params);

    // Step 2: Lower the plan step to an executable step
    let plan = make_test_plan(vec![plan_step]);
    let executable_steps =
        lower_plan_to_executable_steps(&plan).expect("Plan lowering should succeed");
    assert_eq!(executable_steps.len(), 1);
    let step = &executable_steps[0];

    // Verify the step has a real BashAction
    assert!(
        step.action.is_bash(),
        "Should have real BashAction for bash action step"
    );

    let result = match step.inner_action() {
        ExecutableAction::Bash(action) => {
            execute_bash_action(action, &ShellSandboxConfig::default(), None, None)
                .await
                .expect("Direct execution should succeed for bash action")
        },
        other => panic!("Expected bash action, got {:?}", other),
    };

    // Step 5: Verify the result - bash commands return ActionResult::Text
    match result {
        ActionResult::Text { content } => {
            assert!(
                content.contains("direct_exec_bash_test"),
                "Output should contain the echo'd text: {}",
                content
            );
        },
        other => panic!("Expected ActionResult::Text, got: {:?}", other),
    }
}

/// Test that an HTTP action executes correctly through direct execution path.
/// Uses a local mock server to avoid external dependencies.
#[tokio::test]
async fn test_observe_act_loop_native_http_action_through_execute_step_as_goal() {
    use actix_web::{web, App, HttpResponse, HttpServer};
    use std::net::TcpListener;

    // Step 1: Start a simple mock HTTP server
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!(
                "Skipping test_observe_act_loop_native_http_action_through_execute_step_as_goal: TCP bind not permitted in this environment ({})",
                err
            );
            return;
        },
        Err(err) => panic!("Failed to bind: {}", err),
    };
    let addr = listener.local_addr().expect("Failed to get address");
    let server_url = format!("http://{}/test-endpoint", addr);

    let server = HttpServer::new(|| {
        App::new().route(
            "/test-endpoint",
            web::get().to(|| async {
                HttpResponse::Ok()
                    .content_type("application/json")
                    .body(r#"{"message": "direct_exec_http_test_response"}"#)
            }),
        )
    })
    .listen(listener)
    .expect("Failed to listen")
    .run();

    // Spawn server in background
    let server_handle = tokio::spawn(server);

    // Give server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Step 2: Create an http_get step
    let mut params = HashMap::new();
    params.insert("url".to_string(), server_url.clone());
    let plan_step = make_test_step("step-direct-http", "http_get", params);

    // Step 3: Lower the plan step to an executable step
    let plan = make_test_plan(vec![plan_step]);
    let executable_steps =
        lower_plan_to_executable_steps(&plan).expect("Plan lowering should succeed");
    assert_eq!(executable_steps.len(), 1);
    let step = &executable_steps[0];

    // Verify the step has a real HttpAction
    assert!(
        step.action.is_http(),
        "Should have real HttpAction for HTTP action step"
    );

    let result = match step.inner_action() {
        ExecutableAction::Http(action) => execute_http_action(action, None)
            .await
            .expect("Direct execution should succeed for HTTP action"),
        other => {
            server_handle.abort();
            panic!("Expected http action, got {:?}", other);
        },
    };

    // Step 6: Verify the result - HTTP requests return ActionResult::Http
    match result {
        ActionResult::Http {
            status,
            headers: _,
            body,
        } => {
            assert!(
                body.contains("direct_exec_http_test_response"),
                "Response should contain expected message: {}",
                body
            );
            assert_eq!(status, 200, "HTTP status should be 200");
        },
        other => panic!("Expected ActionResult::Http, got: {:?}", other),
    }

    // Cleanup: abort server
    server_handle.abort();
}

/// Test that a browser step does NOT use native execution.
/// Browser work is delegated to the browser inner loop.
#[test]
fn test_observe_act_loop_browser_step_does_not_use_native_path() {
    // Create a browser navigate step
    let mut params = HashMap::new();
    params.insert("url".to_string(), "https://example.com".to_string());
    let plan_step = make_test_step("step-browser", "browser_navigate", params);

    let plan = make_test_plan(vec![plan_step]);
    let executable_steps =
        lower_plan_to_executable_steps(&plan).expect("Plan lowering should succeed");

    assert_browser_pack_action(executable_steps[0].inner_action(), "navigate");

    // Verify the tool detection functions
    assert!(is_browser_tool("browser"));
    assert!(!is_file_tool("browser"));
    assert!(!is_shell_tool("browser"));
    assert!(!is_http_tool("browser"));
}

/// Test that StepExecutionResult with executed_action serializes and deserializes correctly.
#[test]
fn test_step_execution_result_serialization_with_native_action() {
    use magician::magician_v2::execution::{ActionValidation, StepExecutionResult, StepStatus};
    use serde_json;

    // Create a StepExecutionResult with executed_action populated (unified field)
    let native_action = ExecutableAction::File(FileAction::Write {
        path: std::path::PathBuf::from("/tmp/native_action_test.txt"),
        content: "native action test content".to_string(),
        create_dirs: true,
    });

    let result = StepExecutionResult {
        step_id: "step-native-action-test".to_string(),
        status: StepStatus::Completed,
        output: serde_json::json!({"success": true}),
        error: None,
        duration_ms: 42,
        retry_count: 0,
        recovery_strategy: None,
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        executed_action: Some(native_action), // Unified field for all action types
        pre_action_state: None,
        page_observation: None,
        validation: Some(ActionValidation {
            succeeded: true,
            confidence: 1.0,
            detected_changes: vec![],
            expected_state: None,
            actual_state: None,
            reasoning: Some("Test".to_string()),
        }),
        success_confidence: 1.0,
        derived_executions: vec![],
    };

    // Serialize to JSON
    let json = serde_json::to_string_pretty(&result).expect("StepExecutionResult should serialize");

    // Verify executed_action is in the JSON
    assert!(
        json.contains("executed_action"),
        "JSON should contain executed_action field"
    );
    assert!(
        json.contains("/tmp/native_action_test.txt"),
        "JSON should contain the file path"
    );

    // Deserialize back
    let deserialized: StepExecutionResult =
        serde_json::from_str(&json).expect("StepExecutionResult should deserialize");

    // Verify executed_action is preserved
    assert!(
        deserialized.executed_action.is_some(),
        "executed_action should survive round-trip"
    );

    let native = deserialized.executed_action.unwrap();
    assert!(native.is_file(), "Should be FileAction");

    if let ExecutableAction::File(FileAction::Write { path, content, .. }) = native {
        assert_eq!(path.to_string_lossy(), "/tmp/native_action_test.txt");
        assert_eq!(content, "native action test content");
    } else {
        panic!("Expected FileAction::Write");
    }
}
