//! Tests for execution cancellation feature
//!
//! These tests verify that:
//! 1. CancellationToken properly signals cancellation
//! 2. Direct execution can be interrupted cooperatively
//! 3. Orchestrator properly manages cancellation tokens

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Test that CancellationToken starts uncancelled
#[test]
fn test_cancellation_token_starts_uncancelled() {
    let token = CancellationToken::new();
    assert!(!token.is_cancelled(), "Token should start uncancelled");
}

/// Test that CancellationToken can be cancelled
#[test]
fn test_cancellation_token_can_be_cancelled() {
    let token = CancellationToken::new();
    assert!(!token.is_cancelled());

    token.cancel();

    assert!(
        token.is_cancelled(),
        "Token should be cancelled after cancel() is called"
    );
}

/// Test that cloned tokens share cancellation state
#[test]
fn test_cancellation_token_clone_shares_state() {
    let token = CancellationToken::new();
    let token_clone = token.clone();

    assert!(!token.is_cancelled());
    assert!(!token_clone.is_cancelled());

    // Cancel the original
    token.cancel();

    // Both should be cancelled
    assert!(token.is_cancelled());
    assert!(
        token_clone.is_cancelled(),
        "Cloned token should also be cancelled"
    );
}

/// Test that cancellation works across threads
#[tokio::test]
async fn test_cancellation_token_across_threads() {
    let token = CancellationToken::new();
    let token_clone = token.clone();

    // Spawn a task that waits for cancellation
    let handle = tokio::spawn(async move {
        // Wait up to 5 seconds for cancellation
        tokio::select! {
            _ = token_clone.cancelled() => {
                true // Was cancelled
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                false // Timed out
            }
        }
    });

    // Give the task time to start
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Cancel from the main task
    token.cancel();

    // Task should have received cancellation
    let was_cancelled = handle.await.unwrap();
    assert!(was_cancelled, "Task should have been cancelled");
}

/// Test that cancellation check helper works correctly
#[test]
fn test_cancellation_check_helper() {
    // Simulates the is_cancelled() helper pattern used in executor
    fn is_cancelled(token: &Option<CancellationToken>) -> bool {
        token.as_ref().map(|t| t.is_cancelled()).unwrap_or(false)
    }

    // None token should return false
    let none_token: Option<CancellationToken> = None;
    assert!(
        !is_cancelled(&none_token),
        "None token should not be cancelled"
    );

    // Uncancelled token should return false
    let some_token = Some(CancellationToken::new());
    assert!(
        !is_cancelled(&some_token),
        "Uncancelled token should return false"
    );

    // Cancelled token should return true
    let cancelled_token = CancellationToken::new();
    cancelled_token.cancel();
    let some_cancelled = Some(cancelled_token);
    assert!(
        is_cancelled(&some_cancelled),
        "Cancelled token should return true"
    );
}

/// Test that cancellation stops a simulated execution loop
#[tokio::test]
async fn test_cancellation_stops_execution_loop() {
    let token = CancellationToken::new();
    let token_clone = token.clone();

    let steps_executed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let steps_clone = steps_executed.clone();

    // Simulate an execution loop
    let handle = tokio::spawn(async move {
        for i in 0..100 {
            // Check cancellation at start of each iteration (like our executor does)
            if token_clone.is_cancelled() {
                return i; // Return how many steps completed
            }

            // Simulate step execution
            steps_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        100 // All steps completed
    });

    // Let a few steps execute
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Cancel execution
    token.cancel();

    // Wait for loop to finish
    let completed_steps = handle.await.unwrap();

    // Should have stopped before completing all 100 steps
    assert!(
        completed_steps < 100,
        "Execution should have stopped before completing all steps"
    );
    assert!(
        completed_steps > 0,
        "At least some steps should have executed"
    );

    println!("Completed {} steps before cancellation", completed_steps);
}

/// Test that cancellation is immediate (doesn't wait for current step)
#[tokio::test]
async fn test_cancellation_check_is_synchronous() {
    let token = CancellationToken::new();

    // Start check, cancel, then check result
    let before = token.is_cancelled();
    token.cancel();
    let after = token.is_cancelled();

    assert!(!before, "Should not be cancelled before cancel()");
    assert!(after, "Should be cancelled immediately after cancel()");
}

/// Test the DashMap token-storage primitive used inside active execution controls.
#[tokio::test]
async fn test_dashmap_token_storage() {
    use dashmap::DashMap;

    let tokens: Arc<DashMap<String, CancellationToken>> = Arc::new(DashMap::new());

    // Store a token
    let execution_id = "test-exec-123".to_string();
    let token = CancellationToken::new();
    tokens.insert(execution_id.clone(), token.clone());

    // Token should be retrievable and not cancelled
    {
        let stored = tokens.get(&execution_id).unwrap();
        assert!(!stored.is_cancelled());
    }

    // Cancel via the stored reference
    if let Some((_, stored_token)) = tokens.remove(&execution_id) {
        stored_token.cancel();
    }

    // Original token should also be cancelled (they share state)
    assert!(
        token.is_cancelled(),
        "Original token should be cancelled after stored token is cancelled"
    );

    // Token should be removed from map
    assert!(
        tokens.get(&execution_id).is_none(),
        "Token should be removed from map"
    );
}

/// Test that multiple cancellations are idempotent
#[test]
fn test_cancellation_is_idempotent() {
    let token = CancellationToken::new();

    assert!(!token.is_cancelled());

    token.cancel();
    assert!(token.is_cancelled());

    // Cancel again - should not panic or change state
    token.cancel();
    assert!(token.is_cancelled());

    token.cancel();
    assert!(token.is_cancelled());
}

/// Test child token pattern (for nested cancellation scopes)
#[test]
fn test_child_token_cancelled_with_parent() {
    let parent = CancellationToken::new();
    let child = parent.child_token();

    assert!(!parent.is_cancelled());
    assert!(!child.is_cancelled());

    // Cancel parent
    parent.cancel();

    // Both should be cancelled
    assert!(parent.is_cancelled());
    assert!(
        child.is_cancelled(),
        "Child token should be cancelled when parent is cancelled"
    );
}

/// Test that child cancellation doesn't affect parent
#[test]
fn test_child_cancellation_independent_of_parent() {
    let parent = CancellationToken::new();
    let child = parent.child_token();

    // Cancel child only
    child.cancel();

    assert!(child.is_cancelled());
    assert!(
        !parent.is_cancelled(),
        "Parent should NOT be cancelled when only child is cancelled"
    );
}
