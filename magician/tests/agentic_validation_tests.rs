//! Agentic Execution Validation and Integration Tests
//!
//! These tests verify the input interpretation, pause/resume flow, and multi-step
//! input handling for the agentic execution engine as specified in AGENTIC_EXECUTION_DESIGN.md.
//!
//! Test Categories:
//! 1. Input Interpreter Validation Tests - LLM interpretation, classification parsing, edge cases
//! 2. Agentic Pause/Resume Flow Tests - WaitingForUser outcome, FullPauseStore, resume with validation
//! 3. Multi-Step Input Handling Tests - Multiple inputs, validation failure, re-ask scenarios

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use serde_json::Value;

use magician::magician_v2::execution::agentic::types::CompletionKind;
use magician::magician_v2::execution::agentic::{
    try_simple_extraction, AgenticContext, AgenticOutcome, AgenticPauseState, ChoiceOption,
    EnvironmentState, ExecutionHistory, FilesystemState, FullPauseStore, HttpState,
    InterpretationResult, PendingInput, PendingInputSource, ShellState, UserInputResponse,
    UserInputType, UserInputValue,
};
use magician::magician_v2::execution::{BashAction, ExecutableAction, PageState};

// ============================================================================
// Test Helpers
// ============================================================================

fn create_test_shell_state() -> ShellState {
    ShellState {
        working_dir: PathBuf::from("/tmp/test"),
        last_command: Some("echo hello".to_string()),
        last_stdout: Some("hello\n".to_string()),
        last_stderr: None,
        last_exit_code: Some(0),
    }
}

fn create_test_filesystem_state() -> FilesystemState {
    FilesystemState {
        current_dir: PathBuf::from("/tmp/test"),
        last_operation: Some("Read".to_string()),
        last_result: Some("file content".to_string()),
        error: None,
    }
}

fn create_test_http_state() -> HttpState {
    HttpState {
        last_url: Some("https://api.example.com/data".to_string()),
        last_status: Some(200),
        last_response: Some(r#"{"status": "ok"}"#.to_string()),
        error: None,
        browser_url_hint: None,
    }
}

fn create_test_pause_state() -> AgenticPauseState {
    AgenticPauseState::new(
        3,                                                        // iteration
        "Complete the login flow".to_string(),                    // goal
        "User is logged in and dashboard is visible".to_string(), // success_criteria
        EnvironmentState::Shell(create_test_shell_state()),
        "Iteration 1: Navigated to page\nIteration 2: Clicked login button".to_string(),
        10, // max_iterations
        3,  // max_repeated_actions
    )
    .with_observability("exec-123", "plan-456", "step-789")
}

// ============================================================================
// 1. INPUT INTERPRETER VALIDATION TESTS
// ============================================================================

mod simple_extraction_tests {
    use super::*;

    #[test]
    fn test_simple_extraction_yes_variants() {
        let input =
            PendingInput::from_planning("test-1", "confirm", None, Some("Confirm?".to_string()));

        // Test all affirmative variants
        for response in &[
            "yes", "y", "ok", "okay", "sure", "confirm", "proceed", "true", "1", "YES", "Yes",
        ] {
            let result = try_simple_extraction(&input, response);
            assert!(result.is_some(), "Should extract from '{}'", response);
            let result = result.unwrap();
            assert!(result.success, "Should be successful for '{}'", response);
            assert_eq!(
                result.value,
                Some(Value::Bool(true)),
                "Should be true for '{}'",
                response
            );
            assert!(
                result.confidence > 0.9,
                "Should have high confidence for '{}'",
                response
            );
        }
    }

    #[test]
    fn test_simple_extraction_no_variants() {
        let input =
            PendingInput::from_planning("test-1", "confirm", None, Some("Confirm?".to_string()));

        // Test all negative variants
        for response in &[
            "no", "n", "cancel", "stop", "false", "0", "nope", "NO", "No",
        ] {
            let result = try_simple_extraction(&input, response);
            assert!(result.is_some(), "Should extract from '{}'", response);
            let result = result.unwrap();
            assert!(result.success, "Should be successful for '{}'", response);
            assert_eq!(
                result.value,
                Some(Value::Bool(false)),
                "Should be false for '{}'",
                response
            );
        }
    }

    #[test]
    fn test_simple_extraction_integers() {
        let input =
            PendingInput::from_planning("test-1", "count", None, Some("How many?".to_string()));

        // Note: "0" and "1" are special cases - they map to bool false/true
        // Only test unambiguous integer values
        for (response, expected) in &[("42", 42i64), ("-5", -5), ("1000", 1000), ("999", 999)] {
            let result = try_simple_extraction(&input, response);
            assert!(
                result.is_some(),
                "Should extract integer from '{}'",
                response
            );
            let result = result.unwrap();
            assert!(result.success);
            assert_eq!(result.value, Some(Value::Number((*expected).into())));
        }
    }

    #[test]
    fn test_simple_extraction_zero_one_are_booleans() {
        // "0" and "1" are interpreted as booleans (false/true) per the implementation
        let input = PendingInput::from_planning("test-1", "value", None, None);

        let result = try_simple_extraction(&input, "0").unwrap();
        assert_eq!(result.value, Some(Value::Bool(false)));

        let result = try_simple_extraction(&input, "1").unwrap();
        assert_eq!(result.value, Some(Value::Bool(true)));
    }

    #[test]
    fn test_simple_extraction_floats() {
        let input =
            PendingInput::from_planning("test-1", "price", None, Some("Enter price".to_string()));

        let result = try_simple_extraction(&input, "3.14");
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.success);
        // Float parsing - check it's a number
        assert!(result
            .value
            .as_ref()
            .map(|v| v.is_number())
            .unwrap_or(false));
    }

    #[test]
    fn test_simple_extraction_email() {
        let input =
            PendingInput::from_planning("test-1", "email", None, Some("Enter email".to_string()));

        let result = try_simple_extraction(&input, "user@example.com");
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(
            result.value,
            Some(Value::String("user@example.com".to_string()))
        );
        assert!(result.confidence >= 0.85);
    }

    #[test]
    fn test_simple_extraction_single_word() {
        let input =
            PendingInput::from_planning("test-1", "username", None, Some("Username".to_string()));

        let result = try_simple_extraction(&input, "johndoe");
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.value, Some(Value::String("johndoe".to_string())));
    }

    #[test]
    fn test_simple_extraction_complex_needs_llm() {
        let input =
            PendingInput::from_planning("test-1", "notes", None, Some("Any notes?".to_string()));

        // Multi-line response needs LLM
        let result = try_simple_extraction(&input, "Here are my notes:\n- Item 1\n- Item 2");
        assert!(
            result.is_none(),
            "Complex multi-line should return None for LLM processing"
        );

        // Long response with spaces needs LLM
        let result = try_simple_extraction(
            &input,
            "This is a longer response that contains spaces and might need semantic interpretation",
        );
        assert!(
            result.is_none(),
            "Long response with spaces should return None for LLM"
        );
    }

    #[test]
    fn test_simple_extraction_empty_string() {
        let input = PendingInput::from_planning("test-1", "value", None, None);

        let result = try_simple_extraction(&input, "");
        // Empty string should be extracted as empty string value
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.value, Some(Value::String("".to_string())));
    }

    #[test]
    fn test_simple_extraction_whitespace_handling() {
        let input = PendingInput::from_planning("test-1", "value", None, None);

        // Should trim whitespace
        let result = try_simple_extraction(&input, "  yes  ");
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.value, Some(Value::Bool(true)));
    }

    #[test]
    fn test_simple_extraction_special_characters() {
        let input =
            PendingInput::from_planning("test-1", "code", None, Some("Enter code".to_string()));

        // Special characters in single-word response
        let result = try_simple_extraction(&input, "ABC-123-XYZ");
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.value, Some(Value::String("ABC-123-XYZ".to_string())));
    }
}

mod interpretation_result_tests {
    use super::*;

    #[test]
    fn test_interpretation_result_success() {
        let result = InterpretationResult::success(
            "input-1".to_string(),
            Value::String("test@example.com".to_string()),
            0.95,
            "Extracted email address".to_string(),
        );

        assert!(result.success);
        assert_eq!(result.input_id, "input-1");
        assert_eq!(
            result.value,
            Some(Value::String("test@example.com".to_string()))
        );
        assert_eq!(result.confidence, 0.95);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_interpretation_result_failure() {
        let result = InterpretationResult::failure(
            "input-1".to_string(),
            "Could not understand the response".to_string(),
        );

        assert!(!result.success);
        assert!(result.value.is_none());
        assert_eq!(result.confidence, 0.0);
    }

    #[test]
    fn test_interpretation_result_with_warnings() {
        let result = InterpretationResult::success(
            "input-1".to_string(),
            Value::String("maybe@example.com".to_string()),
            0.7,
            "Extracted possible email".to_string(),
        )
        .with_warning("Response was ambiguous".to_string());

        assert!(result.success);
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0], "Response was ambiguous");
    }
}

// ============================================================================
// 2. AGENTIC PAUSE/RESUME FLOW TESTS
// ============================================================================

mod pause_state_tests {
    use super::*;

    #[test]
    fn test_pause_state_creation() {
        let env_state = EnvironmentState::Shell(create_test_shell_state());
        let pause_state = AgenticPauseState::new(
            5,
            "Complete login",
            "Dashboard visible",
            env_state,
            "Previous actions summary",
            20,
            3,
        );

        assert_eq!(pause_state.iteration, 5);
        assert_eq!(pause_state.goal, "Complete login");
        assert_eq!(pause_state.success_criteria, "Dashboard visible");
        assert_eq!(pause_state.max_iterations, 20);
        assert_eq!(pause_state.max_repeated_actions, 3);
        assert!(pause_state.execution_id.is_none());
    }

    #[test]
    fn test_pause_state_with_observability() {
        let pause_state = create_test_pause_state();

        assert_eq!(pause_state.execution_id, Some("exec-123".to_string()));
        assert_eq!(pause_state.plan_id, Some("plan-456".to_string()));
        assert_eq!(pause_state.step_id, Some("step-789".to_string()));
    }

    #[test]
    fn test_pause_state_storage_key() {
        let pause_state = create_test_pause_state();
        let key = pause_state.storage_key();

        assert_eq!(key, "exec-123:plan-456:step-789");
    }

    #[test]
    fn test_pause_state_storage_key_without_observability() {
        let env_state = EnvironmentState::Shell(create_test_shell_state());
        let pause_state =
            AgenticPauseState::new(1, "Goal", "Criteria", env_state, "History", 10, 3);

        // Without observability, should generate a UUID
        let key = pause_state.storage_key();
        assert!(!key.is_empty());
        assert!(key.contains('-')); // UUIDs contain dashes
    }

    #[test]
    fn test_pause_state_serialization() {
        let pause_state = create_test_pause_state();

        let json = serde_json::to_string(&pause_state).unwrap();
        assert!(json.contains("\"iteration\":3"));
        assert!(json.contains("\"goal\":\"Complete the login flow\""));
        assert!(json.contains("\"execution_id\":\"exec-123\""));

        let deserialized: AgenticPauseState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.iteration, 3);
        assert_eq!(deserialized.goal, "Complete the login flow");
    }
}

mod full_pause_store_tests {
    use super::*;

    #[test]
    fn test_full_pause_store_new() {
        let store = FullPauseStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_full_pause_store_with_persistence() {
        let store = FullPauseStore::with_persistence("/tmp/test-pause-store");
        assert!(store.is_empty());
    }

    #[test]
    fn test_full_pause_store_new_shared() {
        let store = FullPauseStore::new_shared();
        assert!(store.is_empty());
    }
}

mod user_input_type_tests {
    use super::*;

    #[test]
    fn test_user_input_type_text() {
        let input = UserInputType::Text {
            placeholder: Some("Enter your email".to_string()),
            multiline: false,
        };

        assert_eq!(input.type_name(), "text");
        assert!(input.options_json().is_none());

        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"placeholder\":\"Enter your email\""));
    }

    #[test]
    fn test_user_input_type_password() {
        let input = UserInputType::Password {
            placeholder: Some("Enter password".to_string()),
        };

        assert_eq!(input.type_name(), "password");

        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"type\":\"password\""));
    }

    #[test]
    fn test_user_input_type_choice() {
        let input = UserInputType::Choice {
            options: vec![
                ChoiceOption::new("opt1", "Option 1"),
                ChoiceOption::new("opt2", "Option 2"),
                ChoiceOption::with_description("opt3", "Option 3", "Detailed description"),
            ],
            allow_other: true,
        };

        assert_eq!(input.type_name(), "choice");

        let options_json = input.options_json();
        assert!(options_json.is_some());
        let options_str = options_json.unwrap();
        assert!(options_str.contains("opt1"));
        assert!(options_str.contains("Option 1"));
    }

    #[test]
    fn test_user_input_type_multi_choice() {
        let input = UserInputType::MultiChoice {
            options: vec![
                ChoiceOption::new("a", "Alpha"),
                ChoiceOption::new("b", "Beta"),
            ],
            min_selections: 1,
            max_selections: 2,
        };

        assert_eq!(input.type_name(), "multi_choice");
        assert!(input.options_json().is_some());
    }

    #[test]
    fn test_user_input_type_confirmation() {
        let input = UserInputType::Confirmation {
            confirm_label: Some("Delete".to_string()),
            deny_label: Some("Keep".to_string()),
            destructive: true,
        };

        assert_eq!(input.type_name(), "confirmation");

        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"destructive\":true"));
    }

    #[test]
    fn test_user_input_type_external_action() {
        let input = UserInputType::ExternalAction {
            instructions: "Complete the CAPTCHA on the page".to_string(),
            done_label: Some("I've completed it".to_string()),
        };

        assert_eq!(input.type_name(), "external_action");
    }

    #[test]
    fn test_user_input_type_file_path() {
        let input = UserInputType::FilePath {
            filter: Some("*.pdf".to_string()),
            multiple: true,
        };

        assert_eq!(input.type_name(), "file_path");
    }

    #[test]
    fn test_user_input_type_guidance() {
        let input = UserInputType::Guidance {
            context: Some("Tried clicking but button is disabled".to_string()),
            suggestions: Some(vec![
                "Wait for page to load".to_string(),
                "Try another approach".to_string(),
            ]),
        };

        assert_eq!(input.type_name(), "guidance");
    }
}

mod user_input_value_tests {
    use super::*;

    #[test]
    fn test_user_input_value_text() {
        let value = UserInputValue::text("user@example.com");

        assert_eq!(value.as_text(), Some("user@example.com"));
        assert!(value.as_password().is_none());
        assert!(!value.is_aborted());
    }

    #[test]
    fn test_user_input_value_password() {
        let value = UserInputValue::password("secret123");

        assert!(value.as_text().is_none());
        assert_eq!(value.as_password(), Some("secret123"));
        assert!(!value.is_aborted());
    }

    #[test]
    fn test_user_input_value_confirmation() {
        let confirmed = UserInputValue::confirmed(true);
        let denied = UserInputValue::confirmed(false);

        match confirmed {
            UserInputValue::Confirmation { confirmed } => assert!(confirmed),
            _ => panic!("Expected Confirmation"),
        }

        match denied {
            UserInputValue::Confirmation { confirmed } => assert!(!confirmed),
            _ => panic!("Expected Confirmation"),
        }
    }

    #[test]
    fn test_user_input_value_aborted() {
        let value = UserInputValue::aborted(Some("User cancelled".to_string()));

        assert!(value.is_aborted());

        match value {
            UserInputValue::Aborted { reason } => {
                assert_eq!(reason, Some("User cancelled".to_string()));
            },
            _ => panic!("Expected Aborted"),
        }
    }

    #[test]
    fn test_user_input_value_choice() {
        let value = UserInputValue::Choice {
            selected_id: "opt1".to_string(),
            other_value: None,
        };

        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("\"selected_id\":\"opt1\""));
    }

    #[test]
    fn test_user_input_value_choice_with_other() {
        let value = UserInputValue::Choice {
            selected_id: "other".to_string(),
            other_value: Some("Custom option".to_string()),
        };

        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("\"other_value\":\"Custom option\""));
    }

    #[test]
    fn test_user_input_value_multi_choice() {
        let value = UserInputValue::MultiChoice {
            selected_ids: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        };

        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("\"selected_ids\""));
    }

    #[test]
    fn test_user_input_value_file_path() {
        let value = UserInputValue::FilePath {
            paths: vec![
                "/path/to/file1.pdf".to_string(),
                "/path/to/file2.pdf".to_string(),
            ],
        };

        match value {
            UserInputValue::FilePath { paths } => {
                assert_eq!(paths.len(), 2);
            },
            _ => panic!("Expected FilePath"),
        }
    }

    #[test]
    fn test_user_input_response_creation() {
        let input_type = UserInputType::Text {
            placeholder: None,
            multiline: false,
        };
        let value = UserInputValue::text("test value");

        let response = UserInputResponse::new(input_type.clone(), value);

        assert_eq!(response.input_type, input_type);
        assert!(!response.is_aborted());
        assert!(response.timestamp <= Utc::now());
    }

    #[test]
    fn test_user_input_response_aborted() {
        let input_type = UserInputType::Text {
            placeholder: None,
            multiline: false,
        };
        let value = UserInputValue::aborted(None);

        let response = UserInputResponse::new(input_type, value);

        assert!(response.is_aborted());
    }
}

mod agentic_outcome_tests {
    use super::*;

    #[test]
    fn test_agentic_outcome_waiting_for_user() {
        let pause_state = create_test_pause_state();
        let pending_inputs = vec![PendingInput::from_agentic_decision(
            "pwd-input",
            "password",
            Some("step-789".to_string()),
            Some("Enter your password".to_string()),
        )];

        let outcome = AgenticOutcome::WaitingForUser {
            question: "Please enter your password".to_string(),
            input_type: UserInputType::Password { placeholder: None },
            hint: Some("Check your email for 2FA code".to_string()),
            pause_state: Box::new(pause_state.clone()),
            asking_for_parameter: Some("password".to_string()),
            pending_inputs,
            resolved_inputs: HashMap::new(),
            escalation_trigger: None,
        };

        assert!(outcome.is_waiting_for_user());
        assert!(!outcome.is_terminal());
        assert!(!outcome.is_success());
        assert_eq!(outcome.iterations_used(), 3);
        assert!(outcome.pause_state().is_some());
    }

    #[test]
    fn test_agentic_outcome_success() {
        let outcome = AgenticOutcome::Success {
            completion: CompletionKind::Full,
            open: Vec::new(),
            final_state: EnvironmentState::Shell(create_test_shell_state()),
            iterations_used: 5,
            artifacts: vec![],
        };

        assert!(outcome.is_success());
        assert!(outcome.is_terminal());
        assert!(!outcome.is_waiting_for_user());
        assert_eq!(outcome.iterations_used(), 5);
        assert!(outcome.pause_state().is_none());
    }

    #[test]
    fn test_agentic_outcome_failed() {
        let outcome = AgenticOutcome::Failed {
            reason: "Element not found".to_string(),
            last_state: EnvironmentState::Shell(create_test_shell_state()),
            iterations_used: 7,
        };

        assert!(!outcome.is_success());
        assert!(outcome.is_terminal());
        assert_eq!(outcome.iterations_used(), 7);
    }

    #[test]
    fn test_agentic_outcome_max_iterations_without_pause_state() {
        let outcome = AgenticOutcome::MaxIterationsReached {
            last_state: EnvironmentState::Shell(create_test_shell_state()),
            iterations_used: 10,
            pause_state: None,
        };

        assert!(!outcome.is_success());
        assert!(outcome.is_terminal());
        assert_eq!(outcome.iterations_used(), 10);
    }

    #[test]
    fn test_agentic_outcome_max_iterations_with_pause_state() {
        let ps = create_test_pause_state();
        let outcome = AgenticOutcome::MaxIterationsReached {
            last_state: EnvironmentState::Shell(create_test_shell_state()),
            iterations_used: 10,
            pause_state: Some(ps),
        };

        assert!(!outcome.is_success());
        assert!(!outcome.is_terminal()); // NOT terminal when pause_state is present
        assert_eq!(outcome.iterations_used(), 10);
        assert!(outcome.pause_state().is_some());
    }

    #[test]
    fn test_agentic_outcome_loop_detected() {
        let outcome = AgenticOutcome::LoopDetected {
            detection_type: "state_loop".to_string(),
            repeated_action: "browser:click(#submit)".to_string(),
            recommendation: "Try a different approach".to_string(),
            last_state: EnvironmentState::Shell(create_test_shell_state()),
            iterations_used: 6,
            cycle_pattern: None,
            similarity: Some(0.95),
        };

        assert!(!outcome.is_success());
        assert!(outcome.is_terminal());
    }
}

// ============================================================================
// 3. MULTI-STEP INPUT HANDLING AND VALIDATION TESTS
// ============================================================================

mod pending_input_tests {
    use super::*;

    #[test]
    fn test_pending_input_from_planning() {
        let input = PendingInput::from_planning(
            "input-1",
            "email",
            Some("step-1".to_string()),
            Some("Enter your email address".to_string()),
        );

        assert_eq!(input.id, "input-1");
        assert_eq!(input.parameter, "email");
        assert_eq!(input.step_id, Some("step-1".to_string()));
        assert_eq!(
            input.description,
            Some("Enter your email address".to_string())
        );
        assert_eq!(input.source, PendingInputSource::DeferredFromPlanning);
        assert!(input.resolved_value.is_none());
        assert!(!input.is_resolved());
    }

    #[test]
    fn test_pending_input_from_execution() {
        let input = PendingInput::from_execution(
            "input-2",
            "api_key",
            None,
            Some("API key needed".to_string()),
        );

        assert_eq!(input.source, PendingInputSource::DiscoveredDuringExecution);
        assert!(input.step_id.is_none());
    }

    #[test]
    fn test_pending_input_from_agentic_decision() {
        let input = PendingInput::from_agentic_decision(
            "input-3",
            "password",
            Some("step-2".to_string()),
            Some("Password required for login".to_string()),
        );

        assert_eq!(input.source, PendingInputSource::AgenticDecision);
    }

    #[test]
    fn test_pending_input_resolve() {
        let mut input = PendingInput::from_planning("input-1", "email", None, None);

        assert!(!input.is_resolved());

        input.resolve(Value::String("test@example.com".to_string()));

        assert!(input.is_resolved());
        assert_eq!(
            input.resolved_value,
            Some(Value::String("test@example.com".to_string()))
        );
    }

    #[test]
    fn test_pending_input_source_serialization() {
        let sources = vec![
            PendingInputSource::DeferredFromPlanning,
            PendingInputSource::DiscoveredDuringExecution,
            PendingInputSource::AgenticDecision,
        ];

        for source in sources {
            let json = serde_json::to_string(&source).unwrap();
            let deserialized: PendingInputSource = serde_json::from_str(&json).unwrap();
            assert_eq!(source, deserialized);
        }
    }
}

mod agentic_context_tests {
    use super::*;

    #[test]
    fn test_context_creation() {
        let ctx = AgenticContext::new("Complete the task", "Task is marked done")
            .with_max_iterations(15)
            .with_max_repeated_actions(4);

        assert_eq!(ctx.goal, "Complete the task");
        assert_eq!(ctx.success_criteria, "Task is marked done");
        assert_eq!(ctx.max_iterations, 15);
        assert_eq!(ctx.max_repeated_actions, 4);
        assert!(ctx.pending_inputs.is_empty());
        assert!(ctx.resolved_inputs.is_empty());
    }

    #[test]
    fn test_context_with_observability() {
        let ctx = AgenticContext::new("Goal", "Criteria")
            .with_observability("exec-1", "plan-1", "step-1");

        assert!(ctx.has_observability());
        assert_eq!(ctx.execution_id, Some("exec-1".to_string()));
        assert_eq!(ctx.legacy_execution_id, Some("exec-1".to_string()));
        assert_eq!(ctx.plan_id, Some("plan-1".to_string()));
        assert_eq!(ctx.step_id, Some("step-1".to_string()));
    }

    #[test]
    fn test_context_pending_inputs() {
        let inputs = vec![
            PendingInput::from_planning("input-1", "email", Some("step-1".to_string()), None),
            PendingInput::from_planning("input-2", "password", Some("step-1".to_string()), None),
            PendingInput::from_planning("input-3", "api_key", Some("step-2".to_string()), None),
        ];

        let ctx = AgenticContext::new("Goal", "Criteria").with_pending_inputs(inputs);

        assert_eq!(ctx.pending_inputs.len(), 3);

        // Get pending inputs for step-1
        let step1_inputs = ctx.get_pending_inputs_for_step("step-1");
        assert_eq!(step1_inputs.len(), 2);

        // Get pending inputs for step-2
        let step2_inputs = ctx.get_pending_inputs_for_step("step-2");
        assert_eq!(step2_inputs.len(), 1);
    }

    #[test]
    fn test_context_resolve_input() {
        let inputs = vec![
            PendingInput::from_planning("input-1", "email", Some("step-1".to_string()), None),
            PendingInput::from_planning("input-2", "password", Some("step-1".to_string()), None),
        ];

        let mut ctx = AgenticContext::new("Goal", "Criteria").with_pending_inputs(inputs);

        assert!(!ctx.is_input_resolved("input-1"));
        assert_eq!(ctx.get_unresolved_inputs().len(), 2);

        // Resolve first input
        ctx.record_resolved_input(
            "input-1".to_string(),
            Value::String("test@example.com".to_string()),
        );

        assert!(ctx.is_input_resolved("input-1"));
        assert!(!ctx.is_input_resolved("input-2"));
        assert_eq!(ctx.get_unresolved_inputs().len(), 1);

        // Get resolved value
        let resolved = ctx.get_resolved_input("input-1");
        assert_eq!(
            resolved,
            Some(&Value::String("test@example.com".to_string()))
        );
    }

    #[test]
    fn test_context_format_pending_inputs_for_llm() {
        // The format_pending_inputs_for_llm function now returns empty string
        // because we use observation-based approach: the agent observes the page
        // and asks for inputs when it encounters them (e.g., password fields)
        // rather than being told upfront about "missing information"
        let inputs = vec![
            PendingInput::from_planning(
                "input-1",
                "email",
                Some("step-1".to_string()),
                Some("User's email address".to_string()),
            ),
            PendingInput::from_planning(
                "input-2",
                "password",
                Some("step-1".to_string()),
                Some("User's password".to_string()),
            ),
        ];

        let ctx = AgenticContext::new("Goal", "Criteria").with_pending_inputs(inputs);

        let formatted = ctx.format_pending_inputs_for_llm("step-1");

        // Returns empty - agent uses observation-based approach instead
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_context_format_resolved_inputs_for_llm() {
        let inputs = vec![
            PendingInput::from_planning("input-1", "email", None, None),
            PendingInput::from_planning("input-2", "password", None, None),
        ];

        let mut ctx = AgenticContext::new("Goal", "Criteria").with_pending_inputs(inputs);

        ctx.record_resolved_input(
            "input-1".to_string(),
            Value::String("test@example.com".to_string()),
        );
        ctx.record_resolved_input(
            "input-2".to_string(),
            Value::String("secret123".to_string()),
        );

        let formatted = ctx.format_resolved_inputs_for_llm();

        assert!(formatted.contains("USER-PROVIDED VALUES"));
        assert!(formatted.contains("email"));
        assert!(formatted.contains("test@example.com"));
        // Password should be redacted with param ID
        assert!(formatted.contains("[REDACTED:input-2]"));
        assert!(!formatted.contains("secret123"));
    }

    #[test]
    fn test_context_format_resolved_inputs_redacts_secrets() {
        let inputs = vec![
            PendingInput::from_planning("input-1", "api_token", None, None),
            PendingInput::from_planning("input-2", "secret_key", None, None),
            PendingInput::from_planning("input-3", "user_password", None, None),
        ];

        let mut ctx = AgenticContext::new("Goal", "Criteria").with_pending_inputs(inputs);

        ctx.record_resolved_input("input-1".to_string(), Value::String("tok_123".to_string()));
        ctx.record_resolved_input("input-2".to_string(), Value::String("sk_456".to_string()));
        ctx.record_resolved_input("input-3".to_string(), Value::String("pwd789".to_string()));

        let formatted = ctx.format_resolved_inputs_for_llm();

        // All should be redacted with their param IDs
        assert!(!formatted.contains("tok_123"));
        assert!(!formatted.contains("sk_456"));
        assert!(!formatted.contains("pwd789"));
        assert!(formatted.contains("[REDACTED:input-1]"));
        assert!(formatted.contains("[REDACTED:input-2]"));
        assert!(formatted.contains("[REDACTED:input-3]"));
    }
}

mod full_pause_data_tests {
    // Note: FullPauseData tests require ActionExecutors which needs LLM service.
    // These tests are covered in integration tests that can spin up the full stack.
    // Unit tests focus on the types that can be tested in isolation.
}

mod environment_state_tests {
    use super::*;

    #[test]
    fn test_environment_state_type_name() {
        let browser = EnvironmentState::Browser(PageState::default());
        let filesystem = EnvironmentState::Filesystem(create_test_filesystem_state());
        let http = EnvironmentState::Http(create_test_http_state());
        let shell = EnvironmentState::Shell(create_test_shell_state());

        assert_eq!(browser.type_name(), "browser");
        assert_eq!(filesystem.type_name(), "filesystem");
        assert_eq!(http.type_name(), "http");
        assert_eq!(shell.type_name(), "shell");
    }

    #[test]
    fn test_environment_state_format_for_llm_shell() {
        let shell = EnvironmentState::Shell(create_test_shell_state());
        let formatted = shell.format_for_llm();

        assert!(formatted.contains("Working Directory: /tmp/test"));
        assert!(formatted.contains("Last Command: echo hello"));
        assert!(formatted.contains("Exit Code: 0"));
        assert!(formatted.contains("Stdout:"));
        assert!(formatted.contains("hello"));
    }

    #[test]
    fn test_environment_state_format_for_llm_filesystem() {
        let fs = EnvironmentState::Filesystem(create_test_filesystem_state());
        let formatted = fs.format_for_llm();

        assert!(formatted.contains("Working Directory: /tmp/test"));
        assert!(formatted.contains("Last Operation: Read"));
        assert!(formatted.contains("file content"));
    }

    #[test]
    fn test_environment_state_format_for_llm_http() {
        let http = EnvironmentState::Http(create_test_http_state());
        let formatted = http.format_for_llm();

        assert!(formatted.contains("Last URL: https://api.example.com/data"));
        assert!(formatted.contains("Status: 200"));
        assert!(formatted.contains("status"));
    }

    #[test]
    fn test_environment_state_format_for_llm_http_empty() {
        let http = EnvironmentState::Http(HttpState::default());
        let formatted = http.format_for_llm();

        assert_eq!(formatted, "No HTTP activity yet");
    }

    #[test]
    fn test_filesystem_state_default() {
        let state = FilesystemState::default();

        assert!(state.last_operation.is_none());
        assert!(state.last_result.is_none());
        assert!(state.error.is_none());
        // current_dir should be the actual current directory or "/"
    }

    #[test]
    fn test_http_state_default() {
        let state = HttpState::default();

        assert!(state.last_url.is_none());
        assert!(state.last_status.is_none());
        assert!(state.last_response.is_none());
        assert!(state.error.is_none());
        assert!(state.browser_url_hint.is_none());
    }

    #[test]
    fn test_shell_state_default() {
        let state = ShellState::default();

        assert!(state.last_command.is_none());
        assert!(state.last_stdout.is_none());
        assert!(state.last_stderr.is_none());
        assert!(state.last_exit_code.is_none());
    }
}

mod execution_history_tests {
    use super::*;

    #[test]
    fn test_execution_history_empty() {
        let history = ExecutionHistory::new();
        assert!(history.iterations.is_empty());

        let formatted = history.format_for_llm(5);
        assert_eq!(formatted, "No actions taken yet.");
    }

    #[test]
    fn test_execution_history_would_repeat_empty() {
        let history = ExecutionHistory::new();
        let action = ExecutableAction::Bash(BashAction::new("ls"));

        assert!(!history.would_repeat(&action, 3));
    }

    // Note: Tests that require constructing IterationRecord with ActionResultRecord
    // are covered in the types.rs unit tests within the crate itself.
    // Integration tests verify the full execution history flow.
}

mod choice_option_tests {
    use super::*;

    #[test]
    fn test_choice_option_new() {
        let option = ChoiceOption::new("opt1", "Option 1");

        assert_eq!(option.id, "opt1");
        assert_eq!(option.label, "Option 1");
        assert!(option.description.is_none());
    }

    #[test]
    fn test_choice_option_with_description() {
        let option = ChoiceOption::with_description("opt1", "Option 1", "This is the first option");

        assert_eq!(option.id, "opt1");
        assert_eq!(option.label, "Option 1");
        assert_eq!(
            option.description,
            Some("This is the first option".to_string())
        );
    }

    #[test]
    fn test_choice_option_serialization() {
        let option = ChoiceOption::with_description("opt1", "Label", "Desc");

        let json = serde_json::to_string(&option).unwrap();
        assert!(json.contains("\"id\":\"opt1\""));
        assert!(json.contains("\"label\":\"Label\""));
        assert!(json.contains("\"description\":\"Desc\""));

        let deserialized: ChoiceOption = serde_json::from_str(&json).unwrap();
        assert_eq!(option.id, deserialized.id);
        assert_eq!(option.label, deserialized.label);
        assert_eq!(option.description, deserialized.description);
    }
}
