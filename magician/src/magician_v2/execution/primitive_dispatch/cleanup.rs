//! Outer-loop terminal cleanup of agent-browser sessions.
//!
//! Every magician execution that opens an agent-browser Chrome window
//! gets the window closed when the execution reaches a terminal outcome
//! — unless ONE of:
//!   1. `AgenticContext::keep_browser_cdp_connection_alive == true`
//!      (LLM signal: follow-up execution will attach to this same
//!      browser; preserve daemon + CDP socket — skip cleanup entirely).
//!   2. `AgenticContext::keep_browser_window_open == true` AND
//!      `keep_browser_cdp_connection_alive == false` (LLM signal:
//!      hand the window off to the human). The runtime invokes
//!      `agent-browser --session <id> close --keep-browser`, which
//!      tells the daemon to shut down without sending `Browser.close`
//!      and without killing the Chrome process group. Chromium
//!      continues running as an ownerless user-visible window.
//!      See `docs/plans/2026-05-24-browser-session-lifecycle-redesign.md`.
//!   3. The outcome is paused/resumable, in which case closing the
//!      window would lose state the next resume needs.
//!
//! Called from `execute_agent_cycle` after `execute_agentically` returns,
//! a single point that fires regardless of which return path the inner
//! function took.

use std::sync::atomic::Ordering;

use tracing::{info, warn};

use super::browser::session::{
    close_session_by_id_with_options, close_session_for_thread,
    close_session_for_thread_with_options, flat_browser_session_is_cdp,
    flat_browser_session_is_headless, flat_browser_sessions_for_thread, AgentBrowserSession,
};
use crate::magician_v2::execution::agentic::{ActionExecutors, AgenticContext, AgenticOutcome};

/// Decide whether `outcome` represents a true end-of-execution (close the
/// window) versus a pause that may resume (keep it open).
pub fn outcome_terminates_browser_session(outcome: &AgenticOutcome) -> bool {
    match outcome {
        // Terminal — execution will not resume on this window.
        AgenticOutcome::Success { .. }
        | AgenticOutcome::Failed { .. }
        | AgenticOutcome::CannotProceed { .. }
        | AgenticOutcome::LoopDetected { .. }
        | AgenticOutcome::BudgetExhausted { .. } => true,

        // MaxIterationsReached: only terminal when it has NO pause state
        // to resume from. With pause_state, the orchestrator may continue
        // the same execution — we'd lose form/scroll/auth state if we
        // closed the window here.
        AgenticOutcome::MaxIterationsReached { pause_state, .. } => pause_state.is_none(),

        // Paused / yielded — caller will resume on the same window.
        AgenticOutcome::WaitingForUser { .. }
        | AgenticOutcome::WaitingForConfirmation { .. }
        | AgenticOutcome::PausedByUser { .. }
        | AgenticOutcome::WaitingForChildren { .. }
        | AgenticOutcome::Sleeping { .. } => false,
    }
}

/// Close the agent-browser session opened during this execution if the
/// outcome warrants it. Best-effort; logs at WARN on failure.
pub async fn cleanup_browser_session_if_done(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    outcome: &AgenticOutcome,
) {
    // No-op when execution didn't open a browser session.
    if !executors.browser_run.session_used.load(Ordering::SeqCst) {
        return;
    }

    // Priority 1: LLM-driven overrides. The inner-loop runner sets
    // these flags when a terminal control call carried
    // `keep_browser_cdp_connection_alive: true` (or legacy alias
    // `keep_browser_session_alive`) and/or
    // `keep_browser_window_open: true`. The agent gets the final say.
    //
    // Routing (after merging the static context flags below):
    //   - cdp_alive=true                       → skip cleanup entirely
    //                                            (daemon + CDP stay alive,
    //                                            next execution attaches).
    //   - window_open=true && !cdp_alive       → invoke
    //                                            `close --keep-browser`:
    //                                            daemon exits, Chromium
    //                                            handed off to user.
    //   - neither                              → fall through to outcome
    //                                            classifier / full close.
    //
    // cdp_alive WINS if both are set (preserving CDP is the more
    // conservative choice — handing off would break the contract that
    // the next agent can attach).
    let cdp_alive = executors
        .browser_run
        .keep_alive_override
        .load(Ordering::SeqCst)
        || ctx.keep_browser_cdp_connection_alive;
    let window_open = executors
        .browser_run
        .window_open_override
        .load(Ordering::SeqCst)
        || ctx.keep_browser_window_open;

    // Reset the LLM-signal atomics for safety on shared executors —
    // a follow-up execution that reuses the same `ActionExecutors`
    // instance should start with a clean slate regardless of which
    // branch we take below.
    executors
        .browser_run
        .keep_alive_override
        .store(false, Ordering::SeqCst);
    executors
        .browser_run
        .window_open_override
        .store(false, Ordering::SeqCst);

    if cdp_alive {
        info!(
            target: "browser_cleanup",
            execution_id = ?ctx.execution_id,
            cdp_alive,
            window_open,
            "Browser session kept alive (keep_browser_cdp_connection_alive=true: daemon + CDP socket preserved)."
        );
        return;
    }

    if window_open {
        drain_headed_hars(ctx, executors).await;
        // Hand off to the user: invoke `close --keep-browser` so the
        // daemon exits without killing Chrome.
        //
        // Use the SAME scope-aware resolver that dispatch uses
        // (`resolve_cli_path_for_scope` with principal + workspace), not
        // the scope-blind `resolve_cli_path`. The workspace skills layer
        // is where `make -C skillshub install-scope` puts the binary —
        // dispatch finds it; if cleanup used the narrower resolver, it
        // would fail to find the same binary and leak the window.
        let cli_path = match AgentBrowserSession::resolve_cli_path_for_scope(
            Some(&ctx.storage_base_path),
            ctx.principal.as_deref(),
            ctx.workspace.as_deref(),
        ) {
            Ok(p) => p,
            Err(err) => {
                warn!(
                    target: "browser_cleanup",
                    error = %err,
                    "Failed to resolve agent-browser CLI for keep-browser handoff; window will leak"
                );
                return;
            },
        };
        let thread_id = ctx
            .execution_id
            .as_deref()
            .or(ctx.legacy_execution_id.as_deref())
            .or(ctx.cycle_id.as_deref())
            .or(ctx.goal_id.as_deref())
            .or(ctx.agent_id.as_deref())
            .unwrap_or("inner-loop")
            .to_string();
        let main_closed = match close_session_for_thread_with_options(&thread_id, &cli_path, true)
            .await
        {
            Ok(()) => {
                info!(
                    target: "browser_cleanup",
                    execution_id = ?ctx.execution_id,
                    thread_id = %thread_id,
                    "Browser session handed off to user (close --keep-browser): daemon exited, Chromium remains visible."
                );
                true
            },
            Err(error) => {
                warn!(
                    target: "browser_cleanup",
                    execution_id = ?ctx.execution_id,
                    thread_id = %thread_id,
                    error = %error,
                    "Failed to invoke agent-browser close --keep-browser; daemon + Chromium may leak"
                );
                false
            },
        };
        let additional_closed = close_additional_browser_sessions(
            ctx,
            executors,
            &cli_path,
            AdditionalSessionDisposition::OwnerHandoff,
        )
        .await;
        if main_closed && additional_closed {
            executors
                .browser_run
                .session_used
                .store(false, Ordering::SeqCst);
        }
        return;
    }

    // Priority 3: outcome classifier. Paused/resumable outcomes keep the
    // window open so resume can land on the same tab state.
    if !outcome_terminates_browser_session(outcome) {
        info!(
            target: "browser_cleanup",
            execution_id = ?ctx.execution_id,
            outcome = %outcome_kind(outcome),
            "Browser session kept alive — outcome is paused/resumable."
        );
        return;
    }

    drain_headed_hars(ctx, executors).await;

    // Resolve CLI path the same way dispatch did — scope-aware, so the
    // workspace skills layer (installed via `make -C skillshub
    // install-scope`) is considered before falling through to extras.
    // Using the scope-blind `resolve_cli_path` would mis-fail when the
    // binary lives only at the workspace path, which is the normal
    // install layout.
    let cli_path = match AgentBrowserSession::resolve_cli_path_for_scope(
        Some(&ctx.storage_base_path),
        ctx.principal.as_deref(),
        ctx.workspace.as_deref(),
    ) {
        Ok(p) => p,
        Err(err) => {
            warn!(
                target: "browser_cleanup",
                error = %err,
                "Failed to resolve agent-browser CLI for cleanup; skipping"
            );
            return;
        },
    };

    // Reuse the same thread_id derivation as PrimitiveExecCtx.
    let thread_id = ctx
        .execution_id
        .as_deref()
        .or(ctx.legacy_execution_id.as_deref())
        .or(ctx.cycle_id.as_deref())
        .or(ctx.goal_id.as_deref())
        .or(ctx.agent_id.as_deref())
        .unwrap_or("inner-loop")
        .to_string();

    let main_closed = match close_session_for_thread(&thread_id, &cli_path).await {
        Ok(()) => {
            info!(
                target: "browser_cleanup",
                execution_id = ?ctx.execution_id,
                outcome = %outcome_kind(outcome),
                thread_id = %thread_id,
                "Closed agent-browser session at execution end."
            );
            true
        },
        Err(err) => {
            warn!(
                target: "browser_cleanup",
                execution_id = ?ctx.execution_id,
                thread_id = %thread_id,
                error = %err,
                "Failed to close agent-browser session at execution end."
            );
            false
        },
    };
    let additional_closed = close_additional_browser_sessions(
        ctx,
        executors,
        &cli_path,
        AdditionalSessionDisposition::Terminal,
    )
    .await;
    if main_closed && additional_closed {
        // Reset only after every session owned by this execution has been
        // released. Failed IDs remain tracked so a later cleanup can retry.
        executors
            .browser_run
            .session_used
            .store(false, Ordering::SeqCst);
    }
}

async fn drain_headed_hars(ctx: &AgenticContext, executors: &ActionExecutors) {
    let enabled = executors.api_router.as_ref().is_some_and(|router| {
        router
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .config()
            .any_enabled()
    });
    if !enabled {
        return;
    }
    let process_enabled = crate::magician_v2::api_mining::switch::runtime_api_mining_config()
        .map(|config| config.enabled)
        .unwrap_or(false);
    if !crate::magician_v2::api_mining::switch::ApiMiningSwitch::effective_from_disk(
        process_enabled,
        &executors.api_mining_base_path,
    ) {
        return;
    }
    let Some(record_id) = ctx
        .execution_id
        .as_deref()
        .or(ctx.legacy_execution_id.as_deref())
        .or(ctx.task_id.as_deref())
    else {
        return;
    };
    let thread_id = ctx
        .execution_id
        .as_deref()
        .or(ctx.legacy_execution_id.as_deref())
        .or(ctx.cycle_id.as_deref())
        .or(ctx.goal_id.as_deref())
        .or(ctx.agent_id.as_deref())
        .unwrap_or("inner-loop");
    let principal = ctx.principal.as_deref().unwrap_or("anonymous");
    let workspace = ctx.workspace.as_deref().unwrap_or("default");
    for session in flat_browser_sessions_for_thread(thread_id) {
        let drained = super::browser::trace_drain::drain_har_for_task(
            &session,
            &ctx.storage_base_path,
            principal,
            workspace,
            record_id,
            executors.secret_store.as_deref(),
            Some(&executors.delivered_secret_values),
        )
        .await;
        if drained.written > 0 {
            info!(
                target: "browser_cleanup",
                session = session.session_id(),
                written = drained.written,
                "Persisted headed/headless HAR traces before browser cleanup"
            );
        }
    }
}

#[derive(Clone, Copy)]
enum AdditionalSessionDisposition {
    Terminal,
    OwnerHandoff,
}

async fn close_additional_browser_sessions(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    cli_path: &std::path::Path,
    disposition: AdditionalSessionDisposition,
) -> bool {
    let session_ids = executors
        .browser_run
        .additional_session_ids
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    let mut all_closed = true;
    for session_id in session_ids {
        // Asked before the close, which forgets the cached session.
        let cdp = flat_browser_session_is_cdp(&session_id).unwrap_or(false);
        let headless = flat_browser_session_is_headless(&session_id).unwrap_or(false);
        let keep_browser =
            additional_session_keeps_browser(&session_id, disposition, cdp, headless);
        match close_session_by_id_with_options(&session_id, cli_path, keep_browser).await {
            Ok(()) => {
                // The owner's Chrome: detach the debugger and close the
                // window the automation opened (Magicutor leaves a tab the
                // owner had open alone). Not on an owner handoff — the
                // window was asked to stay.
                if cdp && matches!(disposition, AdditionalSessionDisposition::Terminal) {
                    release_cdp_thread(executors, &session_id).await;
                }
                crate::magician_v2::content_sources::retrieval::global_retrieval_runtime_state()
                    .revoke_handoff_session(&session_id);
                executors
                    .browser_run
                    .additional_session_ids
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&session_id);
                info!(
                    target: "browser_cleanup",
                    execution_id = ?ctx.execution_id,
                    session_id = %session_id,
                    keep_browser,
                    cdp,
                    "Released browser session this run used beyond its own."
                );
            },
            Err(error) => {
                all_closed = false;
                warn!(
                    target: "browser_cleanup",
                    execution_id = ?ctx.execution_id,
                    session_id = %session_id,
                    keep_browser,
                    error = %error,
                    "Failed to release browser session this run used beyond its own."
                );
            },
        }
    }
    all_closed
}

/// Whether closing `session_id` leaves its browser running (`--keep-browser`:
/// the daemon exits without `Browser.close`).
///
/// Retrieval handoffs keep their own rule. Any other session — a chat
/// thread's shared one — ends with the task by default: a launched browser is
/// closed, the owner's Chrome (CDP) is never sent `Browser.close` (Magicutor
/// detaches it and closes the automation's window instead, see
/// [`release_cdp_thread`]), and a window is handed over only when the task was
/// asked to keep it open — never a headless one, which has no window and would
/// be stranded with no daemon.
fn additional_session_keeps_browser(
    session_id: &str,
    disposition: AdditionalSessionDisposition,
    cdp: bool,
    headless: bool,
) -> bool {
    if session_id.starts_with("retrieval-") {
        return session_id.starts_with("retrieval-cdp-")
            || matches!(disposition, AdditionalSessionDisposition::OwnerHandoff)
                && session_id.starts_with("retrieval-headed-");
    }
    cdp || matches!(disposition, AdditionalSessionDisposition::OwnerHandoff) && !headless
}

/// Detach the owner's Chrome from a finished CDP session: Magicutor closes the
/// window it opened for the thread and clears the extension session (the
/// debugger detaches, and with it the "is being debugged" bar and the
/// extension's running indicator). Best-effort, like the rest of cleanup.
async fn release_cdp_thread(executors: &ActionExecutors, session_id: &str) {
    let Some(magicutor) = executors.browser.as_ref() else {
        return;
    };
    if let Err(error) = magicutor.delete_session(session_id).await {
        warn!(
            target: "browser_cleanup",
            session_id = %session_id,
            error = %error,
            "Failed to release the CDP thread in Magicutor."
        );
    }
}

fn outcome_kind(outcome: &AgenticOutcome) -> &'static str {
    match outcome {
        AgenticOutcome::Success { .. } => "Success",
        AgenticOutcome::Failed { .. } => "Failed",
        AgenticOutcome::MaxIterationsReached { .. } => "MaxIterationsReached",
        AgenticOutcome::LoopDetected { .. } => "LoopDetected",
        AgenticOutcome::WaitingForUser { .. } => "WaitingForUser",
        AgenticOutcome::WaitingForConfirmation { .. } => "WaitingForConfirmation",
        AgenticOutcome::PausedByUser { .. } => "PausedByUser",
        AgenticOutcome::WaitingForChildren { .. } => "WaitingForChildren",
        AgenticOutcome::BudgetExhausted { .. } => "BudgetExhausted",
        AgenticOutcome::CannotProceed { .. } => "CannotProceed",
        AgenticOutcome::Sleeping { .. } => "Sleeping",
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::agentic::{AgenticPauseState, EnvironmentState};

    fn fake_pause_state() -> AgenticPauseState {
        AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Uninitialized,
            "summary",
            10,
            3,
        )
    }

    #[test]
    fn terminal_variants_close() {
        assert!(outcome_terminates_browser_session(
            &AgenticOutcome::Success {
                completion: crate::magician_v2::execution::agentic::types::CompletionKind::Full,
                open: Vec::new(),
                final_state: EnvironmentState::Uninitialized,
                iterations_used: 5,
                artifacts: vec![],
            }
        ));
        assert!(outcome_terminates_browser_session(
            &AgenticOutcome::Failed {
                reason: "x".into(),
                last_state: EnvironmentState::Uninitialized,
                iterations_used: 3,
            }
        ));
        assert!(outcome_terminates_browser_session(
            &AgenticOutcome::CannotProceed {
                reason: "x".into(),
                last_state: EnvironmentState::Uninitialized,
                iterations_used: 3,
            }
        ));
    }

    #[test]
    fn retrieval_session_cleanup_preserves_only_user_owned_browser_processes() {
        use AdditionalSessionDisposition::{OwnerHandoff, Terminal};
        assert!(additional_session_keeps_browser(
            "retrieval-cdp-rh_auth",
            Terminal,
            true,
            false
        ));
        assert!(!additional_session_keeps_browser(
            "retrieval-headless-rh_public",
            Terminal,
            false,
            true
        ));
        assert!(!additional_session_keeps_browser(
            "retrieval-headed-rh_owner",
            Terminal,
            false,
            false
        ));
        assert!(additional_session_keeps_browser(
            "retrieval-headed-rh_owner",
            OwnerHandoff,
            false,
            false
        ));
    }

    /// A chat thread's shared session ends with the task: a launched browser
    /// is closed, the owner's Chrome is never sent `Browser.close` (Magicutor
    /// detaches it instead), and a window stays only when the task was asked
    /// to keep it open — never a windowless one.
    #[test]
    fn a_chat_thread_session_ends_with_the_task_unless_kept() {
        use AdditionalSessionDisposition::{OwnerHandoff, Terminal};
        let chat = "magician-chat-general";
        assert!(!additional_session_keeps_browser(
            chat, Terminal, false, false
        ));
        assert!(additional_session_keeps_browser(
            chat, Terminal, true, false
        ));
        assert!(additional_session_keeps_browser(
            chat,
            OwnerHandoff,
            false,
            false
        ));
        assert!(!additional_session_keeps_browser(
            chat,
            OwnerHandoff,
            false,
            true
        ));
    }

    #[test]
    fn paused_variants_keep_alive() {
        assert!(!outcome_terminates_browser_session(
            &AgenticOutcome::WaitingForChildren {
                child_execution_ids: vec![],
                last_state: EnvironmentState::Uninitialized,
                iterations_used: 1,
                pause_state: Some(Box::new(fake_pause_state())),
            }
        ));
        assert!(!outcome_terminates_browser_session(
            &AgenticOutcome::PausedByUser {
                pause_state: fake_pause_state(),
                iterations_used: 1,
            }
        ));
    }

    #[test]
    fn max_iterations_with_pause_keeps_alive() {
        let with_pause = AgenticOutcome::MaxIterationsReached {
            last_state: EnvironmentState::Uninitialized,
            iterations_used: 400,
            pause_state: Some(fake_pause_state()),
        };
        let without_pause = AgenticOutcome::MaxIterationsReached {
            last_state: EnvironmentState::Uninitialized,
            iterations_used: 400,
            pause_state: None,
        };
        assert!(!outcome_terminates_browser_session(&with_pause));
        assert!(outcome_terminates_browser_session(&without_pause));
    }
}
