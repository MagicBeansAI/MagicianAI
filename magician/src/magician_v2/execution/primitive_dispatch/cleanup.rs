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

use tracing::{debug, info, warn};

use super::browser::session::{
    close_session_by_id_with_options, flat_browser_session_ids_for_thread,
    flat_browser_session_is_cdp, flat_browser_session_is_headless,
    flat_browser_sessions_for_thread, AgentBrowserSession,
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
        | AgenticOutcome::LoopDetected { .. } => true,

        // A budget or provider-failure pause the owner can continue (Retry,
        // more budget) resumes on this window; only an unresumable stop ends it.
        AgenticOutcome::BudgetExhausted { pause_state, .. } => pause_state.is_none(),

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

/// The run key a browser session lease was taken under: the same derivation
/// as `PrimitiveExecCtx::thread_id` (execution id first).
fn lease_run_key(ctx: &AgenticContext) -> &str {
    ctx.execution_id
        .as_deref()
        .or(ctx.legacy_execution_id.as_deref())
        .or(ctx.cycle_id.as_deref())
        .or(ctx.goal_id.as_deref())
        .or(ctx.agent_id.as_deref())
        .unwrap_or("primitive")
}

/// Close the agent-browser session opened during this execution if the
/// outcome warrants it. Best-effort; logs at WARN on failure.
pub async fn cleanup_browser_session_if_done(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    outcome: &AgenticOutcome,
) {
    if !executors.browser_run.session_used.load(Ordering::SeqCst) {
        return;
    }
    // Pauses must preserve both the browser state and pending handoff flags.
    if !outcome_terminates_browser_session(outcome) {
        return;
    }
    let cleanup_key = super::browser::session_lease::assigned_window(lease_run_key(ctx))
        .unwrap_or_else(|| {
            format!(
                "magician-{}",
                super::browser::session::sanitize_session_id(lease_run_key(ctx))
            )
        });
    let _guard = super::browser::session_lease::cleanup_guard(&cleanup_key).await;
    let cdp_alive = executors
        .browser_run
        .keep_alive_override
        .swap(false, Ordering::SeqCst)
        || ctx.keep_browser_cdp_connection_alive;
    let window_open = executors
        .browser_run
        .window_open_override
        .swap(false, Ordering::SeqCst)
        || ctx.keep_browser_window_open;
    if cdp_alive {
        super::browser::session_lease::release_session_users(lease_run_key(ctx));
        // An explicit transfer to a follow-up keeps the existing browser.
        if let Some((window, true)) = super::browser::session_lease::release_run(lease_run_key(ctx))
        {
            super::browser::session_lease::finish_cleanup(&window);
        }
        return;
    }
    drain_headed_hars(ctx, executors).await;
    let cli = match AgentBrowserSession::resolve_cli_path_for_scope(
        Some(&ctx.storage_base_path),
        ctx.principal.as_deref(),
        ctx.workspace.as_deref(),
    ) {
        Ok(cli) => cli,
        Err(error) => {
            warn!(%error, "Browser cleanup cannot resolve its CLI; retaining ownership");
            return;
        },
    };
    let thread = lease_run_key(ctx);
    let base = format!(
        "magician-{}",
        super::browser::session::sanitize_session_id(thread)
    );
    let mut sessions = flat_browser_session_ids_for_thread(&base);
    sessions.extend(super::browser::session_lease::sessions_for_run(thread));
    sessions.extend(
        executors
            .browser_run
            .additional_session_ids
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned(),
    );
    super::browser::session_lease::release_session_users(thread);
    let lease = super::browser::session_lease::release_run(thread);
    if let Some((window, true)) = &lease {
        sessions.push(window.clone());
        sessions.extend(flat_browser_session_ids_for_thread(window));
    }
    if sessions.is_empty() && lease.is_none() {
        sessions.push(base);
    }
    sessions.sort();
    sessions.dedup();
    let disposition = if window_open {
        AdditionalSessionDisposition::OwnerHandoff
    } else {
        AdditionalSessionDisposition::Terminal
    };
    let mut all_closed = true;
    let mut reservations = Vec::new();
    let mut failed = Vec::new();
    for session in sessions {
        if !super::browser::session_lease::begin_cleanup(&session) {
            // A sibling or another launch still owns this browser.
            executors
                .browser_run
                .additional_session_ids
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&session);
            continue;
        }
        reservations.push(session.clone());
        match close_owned_session(&session, &cli, executors.browser.as_deref(), disposition).await {
            Ok(()) => {
                executors
                    .browser_run
                    .additional_session_ids
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&session);
                info!(session, "Released terminal browser session");
            },
            Err(error) => {
                all_closed = false;
                failed.push(session.clone());
                executors
                    .browser_run
                    .additional_session_ids
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(session.clone());
                warn!(session, %error, "Browser cleanup incomplete; retaining session for retry");
            },
        }
    }
    if all_closed {
        for session in reservations {
            super::browser::session_lease::finish_cleanup(&session);
        }
        if let Some((window, true)) = lease {
            super::browser::session_lease::finish_cleanup(&window);
        }
        executors
            .browser_run
            .session_used
            .store(false, Ordering::SeqCst);
    } else {
        if let Some((window, true)) = lease {
            reservations.push(window);
        }
        retry_owned_sessions(
            failed,
            reservations,
            cli,
            executors.browser.as_deref().cloned(),
            disposition,
            cleanup_key,
        );
    }
}

/// Close abandoned shared sessions when terminal settlement happens outside the
/// executor (notably cancellation while waiting for input/children).
pub async fn cleanup_terminal_run(
    run: &str,
    storage: &std::path::Path,
    principal: &str,
    workspace: &str,
    magicutor: &crate::magician_v2::execution::MagicutorClient,
    include_default: bool,
) {
    let assigned = super::browser::session_lease::assigned_window(run);
    let known = super::browser::session_lease::sessions_for_run(run);
    if assigned.is_none() && known.is_empty() && !include_default {
        return;
    }
    let window = assigned.clone().unwrap_or_else(|| {
        format!(
            "magician-{}",
            super::browser::session::sanitize_session_id(run)
        )
    });
    let _guard = super::browser::session_lease::cleanup_guard(&window).await;
    // The executor already chose a disposition and scheduled retries. In
    // particular, do not turn its owner handoff into destructive terminal cleanup.
    if super::browser::session_lease::cleanup_pending(&window) {
        return;
    }
    let cli = match AgentBrowserSession::resolve_cli_path_for_scope(
        Some(storage),
        Some(principal),
        Some(workspace),
    ) {
        Ok(cli) => cli,
        Err(error) => {
            warn!(run, %error, "Terminal browser cleanup cannot resolve CLI");
            return;
        },
    };
    super::browser::session_lease::release_session_users(run);
    let release = super::browser::session_lease::release_run(run);
    if assigned.is_some() && release.is_none() {
        return;
    }
    let mut sessions = flat_browser_session_ids_for_thread(&window);
    sessions.extend(known);
    if assigned.is_none() && sessions.is_empty() {
        // Nothing in this process opened a browser for the run: the default
        // id only guesses at a session a previous process may have left, so a
        // run that never browsed must not retry or keep the id reserved.
        close_unknown_session(run, &window, &cli, magicutor).await;
        return;
    }
    if assigned.is_some() {
        sessions.push(window.clone());
    }
    sessions.sort();
    sessions.dedup();
    let mut closed = true;
    let mut failed = Vec::new();
    let mut reservations = vec![window.clone()];
    for session in sessions {
        if !super::browser::session_lease::begin_cleanup(&session) {
            continue;
        }
        reservations.push(session.clone());
        if let Err(error) = close_owned_session(
            &session,
            &cli,
            Some(magicutor),
            AdditionalSessionDisposition::Terminal,
        )
        .await
        {
            closed = false;
            failed.push(session.clone());
            warn!(run, session, %error, "Terminal browser cleanup incomplete");
        }
    }
    if closed {
        for session in reservations {
            super::browser::session_lease::finish_cleanup(&session);
        }
    } else {
        retry_owned_sessions(
            failed,
            reservations,
            cli,
            Some(magicutor.clone()),
            AdditionalSessionDisposition::Terminal,
            window,
        );
    }
}

/// Best-effort close of a session this process never cached; a missing
/// session is the expected outcome, so failures are only logged at debug.
async fn close_unknown_session(
    run: &str,
    session: &str,
    cli: &std::path::Path,
    magicutor: &crate::magician_v2::execution::MagicutorClient,
) {
    if !super::browser::session_lease::begin_cleanup(session) {
        return;
    }
    if let Err(error) = close_session_by_id_with_options(session, cli, true).await {
        debug!(run, session, %error, "No agent-browser session to close");
    }
    if let Err(error) = magicutor.delete_session_checked(session).await {
        debug!(run, session, %error, "No Magicutor thread to release");
    }
    super::browser::session_lease::finish_cleanup(session);
}

fn retry_owned_sessions(
    mut failed: Vec<String>,
    reservations: Vec<String>,
    cli: std::path::PathBuf,
    magicutor: Option<crate::magician_v2::execution::MagicutorClient>,
    disposition: AdditionalSessionDisposition,
    cleanup_key: String,
) {
    tokio::spawn(async move {
        for delay in [2, 5, 15] {
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            let _guard = super::browser::session_lease::cleanup_guard(&cleanup_key).await;
            let mut remaining = Vec::new();
            for session in failed {
                if !super::browser::session_lease::begin_cleanup(&session) {
                    continue;
                }
                if let Err(error) =
                    close_owned_session(&session, &cli, magicutor.as_ref(), disposition).await
                {
                    warn!(session, %error, "Retrying terminal browser cleanup failed");
                    remaining.push(session);
                }
            }
            if remaining.is_empty() {
                for session in reservations {
                    super::browser::session_lease::finish_cleanup(&session);
                }
                return;
            }
            failed = remaining;
        }
        warn!(
            ?failed,
            "Browser cleanup still incomplete after retries; session IDs remain reserved"
        );
    });
}

async fn close_owned_session(
    session: &str,
    cli: &std::path::Path,
    magicutor: Option<&crate::magician_v2::execution::MagicutorClient>,
    disposition: AdditionalSessionDisposition,
) -> anyhow::Result<()> {
    let cdp = flat_browser_session_is_cdp(session).unwrap_or(
        !session.starts_with("retrieval-headed-") && !session.starts_with("retrieval-headless-"),
    );
    let headless = flat_browser_session_is_headless(session)
        .unwrap_or(session.starts_with("retrieval-headless-"));
    let keep = additional_session_keeps_browser(session, disposition, cdp, headless);
    close_session_by_id_with_options(session, cli, keep).await?;
    if cdp {
        let magicutor = magicutor
            .ok_or_else(|| anyhow::anyhow!("Magicutor client unavailable for CDP cleanup"))?;
        if matches!(disposition, AdditionalSessionDisposition::OwnerHandoff) {
            magicutor.handoff_session_checked(session).await?;
        } else {
            magicutor.delete_session_checked(session).await?;
        }
    }
    crate::magician_v2::content_sources::retrieval::global_retrieval_runtime_state()
        .revoke_handoff_session(session);
    Ok(())
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

/// Whether closing `session_id` leaves its browser running (`--keep-browser`:
/// the daemon exits without `Browser.close`).
///
/// Retrieval handoffs keep their own rule. Any other session — a chat
/// thread's shared one — ends with the task by default: a launched browser is
/// closed, the owner's Chrome (CDP) is never sent `Browser.close` (Magicutor
/// detaches it and closes only automation-created tabs), and a window is handed over only when the task was
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
