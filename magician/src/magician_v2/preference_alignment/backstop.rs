//! The admission backstop in `execute_action_inner` (P4.5, P0 §2 rows 3a, 3b
//! and 3d).
//!
//! Plane `tools/call`, plane approved capture and harness execute-on-resume
//! reach the shared action executor without passing the native loop's gate.
//! The backstop is their admission point: a checked act that the loop did not
//! already admit is judged here, against the same contract, before any side
//! effect.
//!
//! An act the loop admitted carries a registry entry (see
//! [`super::admission`]) and is consumed here once, so the loop's acts are
//! never judged twice. An act whose arguments changed after admission has a
//! different binding, finds no entry and is judged again.
//!
//! A verdict that stops the act is returned typed ([`BackstopVerdict`]). A
//! hold carries its [`HoldSeed`], so a caller that can pause — a harness turn
//! ending on its plane approval gate — asks the owner with the same typed ask
//! the loop uses (P5). A caller that cannot pause refuses the act as a
//! failure, never as a result that reads as success. A cancellation, or an
//! unresolved check the owner may not waive, is always a refusal.

use super::admission::{
    admission_log_entry, append_alignment_log, register_admission, take_admission,
};
use super::classify::classify_action;
use super::collect::{action_binding, CheckRequest};
use super::hold::{AlignmentHeld, HoldSeed};
use super::service::{check_action, current_config, AlignmentAdmission, AlignmentEnvironment};
use crate::config::{PreferenceAlignmentConfig, PreferenceAlignmentMode};
use crate::magician_v2::agents::AgentMemoryResolver;
use crate::magician_v2::execution::actions::ExecutableAction;
use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use crate::magician_v2::taste_profile::TasteProfileLoader;

/// Who the act is for and what it was asked under.
pub struct BackstopScope<'a> {
    pub principal: Option<&'a str>,
    pub workspace: Option<&'a str>,
    pub execution_id: Option<&'a str>,
    /// The run's task text: context for the judge, never owner evidence.
    pub request_text: &'a str,
    /// The authenticated owner's own message, when the run carries one. The
    /// only source of owner spans (P4.8).
    pub owner_turn: Option<&'a super::owner_turn::OwnerTurn>,
}

/// Process handles the backstop needs.
pub struct BackstopEnvironment<'a> {
    pub router: Option<&'a OperationLlmRouter>,
    /// Provider-free judge for the journey lane and tests; `None` in
    /// production.
    pub scripted: Option<&'a dyn super::judge::ScriptedJudge>,
    pub resolver: &'a AgentMemoryResolver,
    pub taste_loader: Option<&'a TasteProfileLoader>,
}

/// The loop admitted this act: record it so the backstop consumes rather than
/// re-judges it. Called at the dispatch, after the fence.
pub fn note_loop_admission(
    principal: &str,
    workspace: &str,
    execution_id: Option<&str>,
    binding_id: &str,
) {
    register_admission(principal, workspace, execution_id, binding_id);
}

/// What the backstop decided for one act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackstopVerdict {
    /// Not checked, admitted already, or nothing stops it.
    Continue,
    /// The act must not run, and no owner answer can change that here.
    Refuse(String),
    /// The act needs the owner's review. `message` is the refusal for a
    /// caller that cannot pause.
    Hold { seed: HoldSeed, message: String },
}

impl BackstopVerdict {
    /// The error a stopped act fails with, for `execute_action`'s callers.
    pub fn into_error(self) -> Option<AlignmentHeld> {
        match self {
            Self::Continue => None,
            Self::Refuse(message) => Some(AlignmentHeld {
                seed: None,
                message,
            }),
            Self::Hold { seed, message } => Some(AlignmentHeld {
                seed: Some(seed),
                message,
            }),
        }
    }
}

/// The verdict for one act under the installed configuration.
pub async fn backstop(
    environment: &BackstopEnvironment<'_>,
    scope: &BackstopScope<'_>,
    action: &ExecutableAction,
) -> BackstopVerdict {
    backstop_with(&current_config(), environment, scope, action).await
}

/// [`backstop`] under an explicit configuration.
pub async fn backstop_with(
    config: &PreferenceAlignmentConfig,
    environment: &BackstopEnvironment<'_>,
    scope: &BackstopScope<'_>,
    action: &ExecutableAction,
) -> BackstopVerdict {
    if config.mode() == PreferenceAlignmentMode::Off {
        return BackstopVerdict::Continue;
    }
    let (Some(principal), Some(workspace)) = (scope.principal, scope.workspace) else {
        return BackstopVerdict::Continue;
    };
    if !classify_action(action).is_checked() {
        return BackstopVerdict::Continue;
    }
    // A checked act that cannot be bound cannot be matched to an admission
    // either; the check below reports it unresolved.
    if let Some(binding_id) = action_binding(action) {
        if take_admission(principal, workspace, scope.execution_id, &binding_id) {
            return BackstopVerdict::Continue;
        }
    }

    let request = CheckRequest::for_run(
        principal,
        workspace,
        scope.execution_id,
        scope.request_text,
        scope.owner_turn,
        0,
    );
    let taste_capture = environment
        .taste_loader
        .and(crate::magician_v2::taste_capture::global_capture_service());
    let check = check_action(
        &AlignmentEnvironment {
            config,
            router: environment.router,
            scripted: environment.scripted,
            resolver: environment.resolver,
            taste_loader: environment.taste_loader,
            taste_capture: taste_capture.as_deref(),
        },
        &request,
        action,
        &[],
    )
    .await;
    if let Some(ticket) = check.ticket(&request) {
        append_alignment_log(
            environment.resolver,
            principal,
            workspace,
            &admission_log_entry(&ticket.record, None),
        )
        .await;
    }
    let Some(message) = refusal_for(&check.admission) else {
        return BackstopVerdict::Continue;
    };
    match HoldSeed::from_check(
        &check,
        action.action_type_name(),
        config.recovery.allow_proceed_without_check,
    ) {
        Some(seed) => BackstopVerdict::Hold { seed, message },
        None => BackstopVerdict::Refuse(message),
    }
}

/// The refusal message for an admission that may not continue. It reaches
/// tool results, chat tool rows, action events and a harness's next turn, so
/// it names no preference and no reason: those stay in the hold's ask, for
/// the owner-private read.
pub fn refusal_for(admission: &AlignmentAdmission) -> Option<String> {
    match admission {
        AlignmentAdmission::NotChecked
        | AlignmentAdmission::Proceed
        | AlignmentAdmission::ProceedWithNudge { .. } => None,
        AlignmentAdmission::Hold { .. } => Some(
            "NOT executed: this action needs the owner's review before it runs. The owner sees \
             why in Magician."
                .to_string(),
        ),
        AlignmentAdmission::Cancel { .. } => {
            Some("NOT executed: the owner cancelled this action.".to_string())
        },
        AlignmentAdmission::Unresolved { .. } => Some(
            "NOT executed: this action could not be checked against the owner's saved \
             preferences. The owner can review it in Magician."
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::preference_alignment::judge::UnresolvedCause;

    #[test]
    fn only_continuing_admissions_pass() {
        assert!(refusal_for(&AlignmentAdmission::Proceed).is_none());
        assert!(refusal_for(&AlignmentAdmission::NotChecked).is_none());
        let refused = refusal_for(&AlignmentAdmission::Unresolved {
            cause: UnresolvedCause::NoRouter,
        })
        .expect("refused");
        assert!(refused.starts_with("NOT executed"));
    }

    #[tokio::test]
    async fn an_act_without_an_owner_scope_or_while_off_is_never_judged() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let environment = BackstopEnvironment {
            router: None,
            scripted: None,
            resolver: &resolver,
            taste_loader: None,
        };
        let send = ExecutableAction::Pack {
            capability_name: "email".to_string(),
            implementation: crate::magician_v2::execution::capability::ImplementationType::Compiled {
                provider_name: "email".to_string(),
            },
            resolved_params: std::collections::HashMap::new(),
        };
        // No owner scope: there are no owner intentions to compare against.
        let unscoped = BackstopScope {
            principal: None,
            workspace: None,
            execution_id: None,
            request_text: "",
            owner_turn: None,
        };
        assert_eq!(
            backstop(&environment, &unscoped, &send).await,
            BackstopVerdict::Continue
        );
    }
}
