//! `Observe` — the phase that used to look at the world.
//!
//! The second phase lifted out of `'iteration_body`. Unlike
//! [`super::prepare`], this one **can end the run**: a browser state reached
//! without the primitive pack is a hard failure, not a re-decide.
//!
//! That is its only explicit exit, though not the reason the function returns
//! `Result` — the observation call it awaits propagates with `?`, so the
//! signature would be `Result` with the explicit exit deleted. What the explicit
//! exit adds is a named failure instead of whatever a disabled path happened to
//! return.
//!
//! # What this phase costs today
//!
//! Almost nothing. Every path hands back `current_state`, unchanged.
//!
//! **Re-capture is retired.** `perform_observation_with_policy` says so in its
//! own body — in the flat loop the post-action state already *is* the
//! observation — and nothing reached from here waits for a page to settle, takes
//! a screenshot, or reads a DOM.
//!
//! What the phase does, then, is pass the current state through and refuse a
//! browser state that has no decision path. Neither is a reason to delete it:
//! the refusal is the run-ending check, and the call is the seam a real
//! observation would return through.
//!
//! # What was deleted from it on 2026-08-27
//!
//! An action-aware policy that was not operating, and the numbers that described
//! what it would have been worth if it were.
//!
//! `should_observe_after_action` consulted exactly one input,
//! `LoopProtectiveState::last_action_context`, and **no site in the workspace
//! ever assigned that field a `Some`** — three cleared it, none filled it. So the
//! policy answered `Full` on every iteration and its `Skip { action_context }`
//! arm was unreachable. The `IterationObservationDecision` it returned reached
//! nothing but the log line at the bottom of this function, which printed
//! `latency_savings_ms` — 2s for `Quick`, 3s for `DomOnly`, 6s for `Skip` —
//! beside it. Those figures modelled a policy that was not running, and they
//! were the only numbers for it in the tree; the older 4–8 second claim in the
//! inline comment here was sourced by nothing at all.
//!
//! The whole surface went together: the policy function, the decision type, the
//! `LastActionContext` it read, the field, and the three sites that cleared it.
//! Nothing about behaviour changed, because nothing was choosing.
//!
//! Two beliefs worth heading off, because they are the natural next guesses and
//! both are wrong:
//!
//! - The decision tag was **not** what the next iteration's policy read. That
//!   policy read `last_action_context` and `last_request_hover_discovery`. The
//!   tag was yielded to the driver until 2026-08-26, where it was assigned to a
//!   local nothing read.
//! - **A stale comment gains authority by being lifted.** The 4–8 second claim
//!   was an aside inside a 4,000-line block that one reader in ten would reach;
//!   at the top of a ninety-line module it became the module's stated reason for
//!   existing, and this doc had grown from it a warning that making observation
//!   unconditional would be a serious regression — a warning about a regression
//!   that cannot occur, since nothing was being skipped.

use anyhow::{anyhow, Result};
use tracing::info;

use crate::magician_v2::execution::agentic::executor::{
    browser_state_is_initial_placeholder, is_blank_page_url, perform_observation_with_policy,
    ActionExecutors, HeapAwaitExt,
};
use crate::magician_v2::execution::agentic::types::{
    AgenticContext, EnvironmentState, LoopProtectiveState,
};

/// What the rest of the iteration reads from `Observe`.
#[derive(Debug, Clone)]
pub struct ObserveOutput {
    /// The state the decision will be made against.
    ///
    /// Owned rather than borrowed because the caller mutates it later in the
    /// iteration — it is `let mut observed_state` in the body.
    pub observed_state: EnvironmentState,
    // The observation decision is deliberately NOT yielded. It used to be, and
    // the driver's only use of it was to assign it to a local with no read
    // anywhere in the workspace — a per-iteration clone of a type that is not
    // `Copy` (`Skip` carried a `String`) paid for nothing. Deleted 2026-08-26
    // along with that local; the type itself followed on 2026-08-27, once its
    // last consumer was one log line in this file.
}

/// Run the observe phase.
///
/// `ctx` is shared, not exclusive: the observation call takes
/// `&AgenticContext` and nothing here writes to it. Per this directory's module
/// docs the parameter list is the checked statement of what a phase may touch,
/// so `Observe` states that it does not mutate the context.
///
/// `browser_primitive_enabled` comes from [`super::prepare::PrepareOutput`] and
/// is deliberately passed in rather than recomputed — see that type for why one
/// value per iteration matters.
pub(in crate::magician_v2::execution::agentic) async fn run(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    loop_protective: &LoopProtectiveState,
    current_state: &EnvironmentState,
    browser_primitive_enabled: bool,
    iteration: usize,
) -> Result<ObserveOutput> {
    // Every branch hands back `current_state`. They are kept apart because the
    // LOG LINES differ and each names a real condition — an initial placeholder,
    // a blank page, a non-browser state — not because the outcomes do.
    let observed_state = if iteration == 1 {
        match current_state {
            EnvironmentState::Browser(page_state)
                if executors.browser.is_some()
                    && browser_state_is_initial_placeholder(page_state) =>
            {
                info!(
                    "[OBS_POLICY] Iteration 1 initial browser state is a placeholder - \
                     handing it to the observation seam"
                );
                perform_observation_with_policy(executors, ctx, current_state, iteration, None)
                    .heap_boxed()
                    .await?
            },
            // First iteration: reuse the provided initial state when it already
            // carries real observation data or when browser execution is not
            // configured.
            _ => current_state.clone(),
        }
    } else {
        match current_state {
            // Blank page: no value in re-observing about:blank. The LLM will
            // immediately decide to navigate.
            EnvironmentState::Browser(ps) if is_blank_page_url(ps.url.as_deref()) => {
                info!(
                    "[OBS_POLICY] Iteration {}: blank page, state passed through",
                    iteration
                );
                current_state.clone()
            },
            EnvironmentState::Browser(_) => {
                perform_observation_with_policy(
                    executors,
                    ctx,
                    current_state,
                    iteration,
                    loop_protective.last_request_hover_discovery,
                )
                .heap_boxed()
                .await?
            },
            // For non-browser contexts, current_state already has the action
            // result from `build_state_from_result` — just reuse it.
            _ => current_state.clone(),
        }
    };

    // The one way this phase ends the run rather than the iteration. Legacy outer
    // browser SoM/text decision paths are disabled, so a browser state reached
    // without the primitive pack has no decision path at all — failing here is
    // the honest answer, and it names the missing pack rather than surfacing
    // whatever the disabled path would have produced.
    if matches!(observed_state, EnvironmentState::Browser(_)) && !browser_primitive_enabled {
        return Err(anyhow!(
            "browser automation requires the `browser` capability pack to declare \
             `implementation.type: primitive`; legacy outer browser SoM/text decision \
             paths are disabled"
        ));
    }

    Ok(ObserveOutput { observed_state })
}
