//! Regression test for the out-of-sandbox file-access → HITL escalation precondition.
//!
//! Audit: `docs/archive/plans/2026-07-10-meta-harness-reliability-audit.md`, P0.4 —
//! "Path/sandbox denial hard-fails on the file-dispatch path agents actually use".
//!
//! The executor's sandbox-override HITL (`ask_sandbox_override_via_user_request_service`,
//! executor.rs) fires only when the file dispatch chokepoint sees the *recoverable*
//! `ExecutionError::PathAccessDenied` variant. The fix made `ensure_path_allowed`
//! (native_executors.rs) return `PathAccessDenied` for a pure out-of-roots miss —
//! distinct from the hard `ExecutionError::Step` used for read-only / delete-policy /
//! repo-fence violations, which must NOT escalate.
//!
//! Driving the full executor requires a live user-request service + orchestrator boot,
//! so these tests guard the *classification precondition* directly on the public
//! `execute_file_action` dispatch path: an out-of-roots valid path yields the
//! recoverable `PathAccessDenied`, while a policy (read-only) violation stays a hard
//! `Step`. If the classification regresses to a bare `Step`, the executor can no longer
//! route it into the sandbox-override HITL — exactly the gap P0.4 closed.

use magician::magician_v2::execution::{execute_file_action, ExecutionError, FileAction};
use runtime_core::{FileSandboxConfig, FileSandboxMode};

/// A valid file path that falls OUTSIDE the configured sandbox roots must be
/// classified as the *recoverable* `ExecutionError::PathAccessDenied` — the
/// precondition the executor chokepoint keys on to raise the sandbox-override
/// HITL (approve folder → merge into `session_file_sandbox_roots` → retry).
#[tokio::test]
async fn out_of_sandbox_read_is_recoverable_path_access_denied() {
    let allowed = tempfile::TempDir::new().expect("Failed to create allowed temp dir");
    // A SECOND real directory so the target resolves cleanly (nearest-existing-parent
    // canonicalization succeeds) yet still lies outside `allowed_roots`.
    let outside = tempfile::TempDir::new().expect("Failed to create outside temp dir");

    let sandbox = FileSandboxConfig {
        mode: FileSandboxMode::WorkspaceWrite,
        allowed_roots: vec![allowed.path().display().to_string()],
        allow_delete: true,
    };

    let action = FileAction::Read {
        path: outside.path().join("secret.txt"),
        encoding: None,
    };

    let err = execute_file_action(&action, &sandbox)
        .await
        .expect_err("read outside allowed roots must be denied");

    match err {
        ExecutionError::PathAccessDenied { paths } => {
            assert!(
                !paths.is_empty(),
                "PathAccessDenied must name the offending path so the HITL can offer it for approval"
            );
            let offending = outside.path().join("secret.txt");
            assert!(
                paths
                    .iter()
                    .any(|p| p.contains("secret.txt") || p == &offending.display().to_string()),
                "expected the out-of-roots target in the denied paths, got {paths:?}"
            );
        },
        other => panic!(
            "expected recoverable ExecutionError::PathAccessDenied (the HITL escalation precondition), got: {other:?}"
        ),
    }
}

/// Contrast guard: a read-only *policy* violation (a mutating action under
/// `FileSandboxMode::ReadOnly`) must remain a HARD `ExecutionError::Step`, NOT
/// the recoverable `PathAccessDenied`. Only the pure out-of-roots case escalates
/// to a sandbox-override HITL; policy denials stay terminal. If this regressed to
/// `PathAccessDenied`, read-only writes would wrongly offer an "approve" prompt.
#[tokio::test]
async fn read_only_policy_violation_stays_hard_step_not_escalatable() {
    let allowed = tempfile::TempDir::new().expect("Failed to create allowed temp dir");

    let sandbox = FileSandboxConfig {
        mode: FileSandboxMode::ReadOnly,
        allowed_roots: vec![allowed.path().display().to_string()],
        allow_delete: true,
    };

    // A write INSIDE the allowed root — so this is purely a mode/policy denial,
    // not an out-of-roots miss.
    let action = FileAction::Write {
        path: allowed.path().join("note.txt"),
        content: "blocked".to_string(),
        create_dirs: false,
    };

    let err = execute_file_action(&action, &sandbox)
        .await
        .expect_err("mutating action under read-only sandbox must be blocked");

    assert!(
        matches!(err, ExecutionError::Step(_)),
        "read-only policy denial must stay a hard Step (non-escalatable), got: {err:?}"
    );
}
